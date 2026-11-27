//! Shared Firm-scoped message and Project inbox.
//!
//! Email and SMS land on one Firm-scoped conversation. A participating Lawyer
//! or Clerk may attribute it to a same-Firm Project after intake. Communication
//! rows stay project-scoped and move with reassignment.
//!
//! Email and SMS matter replies require a Lawyer or Clerk who participates on
//! the assigned Project at dispatch time. Social engagement has its own
//! Firm-scoped authorization path for Owner/Admin and the same participation
//! check for Lawyers/Clerks. A Lawyer approval is not a send. System
//! acknowledgments only confirm persisted intake.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::access::can_see_project;
use crate::application_store::ApplicationStore;
use crate::communications::{self};
use crate::external_identities;
use crate::persons::{self, Role};
use crate::projects::{self, ProjectStoreError};

pub mod channel {
    pub const EMAIL: &str = "email";
    pub const SMS: &str = "sms";
    pub const VOICE: &str = "voice";
    pub const FACEBOOK: &str = "facebook";
    pub const INSTAGRAM: &str = "instagram";
    pub const THREADS: &str = "threads";
    pub const YOUTUBE: &str = "youtube";
    pub const TIKTOK: &str = "tiktok";
    /// Court and program scrapers file their notices here. The Firm inbox
    /// handles them exactly like any other intake: the message carries the
    /// document as an attachment, and triage assigns it.
    pub const SCRAPER: &str = "scraper";
    pub const X: &str = "x";
    /// Imported LinkedIn direct messages (LAW-238 D1). The ingest source is
    /// the member's own data export, not a provider webhook, but the channel
    /// literal, threading, and dedup are exactly the other social channels'.
    pub const LINKEDIN: &str = "linkedin";
}

pub mod direction {
    pub const INBOUND: &str = "inbound";
    pub const OUTBOUND: &str = "outbound";
    pub const INTERNAL: &str = "internal";
}

pub mod delivery {
    pub const RECEIVED: &str = "received";
    pub const QUEUED: &str = "queued";
    pub const SUBMITTED: &str = "submitted";
    pub const DELIVERED: &str = "delivered";
    pub const UNKNOWN: &str = "unknown";
    pub const FAILED: &str = "failed";
    pub const SUPPRESSED: &str = "suppressed";
}

pub mod purpose {
    pub const MATTER: &str = "matter";
    pub const PROMOTIONAL: &str = "promotional";
}

/// The NEF pipeline's recorded outcome spellings. A triage outcome marks a
/// processed message that stays in the lawyer's backlog because nothing was
/// filed — never guess what a notice means.
pub mod nef_outcome {
    /// The notice's documents and docket entry were filed on a matter.
    pub const FILED: &str = "filed";
    /// Available notice documents were filed without a numbered docket entry.
    pub const TEXT_ONLY: &str = "text_only";
    /// A rerun over an entry the pipeline already filed.
    pub const ALREADY_RECORDED: &str = "already_recorded";
    /// The notice names no Project the pipeline could resolve.
    pub const NO_PROJECT: &str = "no_project";
    /// The message is not a notice the pipeline recognizes.
    pub const NOT_A_NOTICE: &str = "not_a_notice";
    /// A recognized NEF without a resolvable case number.
    pub const NO_CASE_NUMBER: &str = "no_case_number";

    /// Outcomes that leave the message in the unprocessed backlog after a
    /// process run records them.
    pub const TRIAGE: &[&str] = &[NO_PROJECT, NOT_A_NOTICE, NO_CASE_NUMBER];

    /// Every outcome spelling, in pipeline order.
    pub const ALL: &[&str] = &[
        FILED,
        TEXT_ONLY,
        ALREADY_RECORDED,
        NO_PROJECT,
        NOT_A_NOTICE,
        NO_CASE_NUMBER,
    ];

    /// Whether a recorded outcome keeps the message in the backlog.
    #[must_use]
    pub fn is_triage(outcome: &str) -> bool {
        TRIAGE.contains(&outcome)
    }
}

/// The inbox-process route vocabulary: the strings a process run records on
/// a message's `route` field, and the schema's ASSERT vocabulary. Portal
/// classification (`portal::inbound_email`) maps its verdict onto these
/// spellings; see `docs/glossary/inbox-process.md`.
pub mod route {
    /// A court-generated Notice of Electronic Filing.
    pub const NEF: &str = "nef";
    /// A bankruptcy claims-agent notice from a recognized noticing vendor.
    pub const CLAIMS_AGENT: &str = "claims_agent";
    /// Mail from a fixed-fee legal plan (ARAG, `LegalEASE`, `LegalShield`).
    pub const PLAN_ASSIGNMENT: &str = "plan_assignment";
    /// Court mail that is not a noticing event, or a hearing invitation.
    pub const CHAMBERS: &str = "chambers";
    /// Anything the classification layer cannot name confidently.
    pub const OTHER: &str = "other";

    /// Every route spelling, in classification order.
    pub const ALL: &[&str] = &[NEF, CLAIMS_AGENT, PLAN_ASSIGNMENT, CHAMBERS, OTHER];

    /// Whether `value` is a route the schema accepts on `message.route`.
    #[must_use]
    pub fn is_valid(value: &str) -> bool {
        ALL.contains(&value)
    }
}

/// LAW-347's first assignment method: match a thread's From/To/Cc addresses
/// against the open Projects' Person emails. Everything here is pure — the
/// portal door resolves the addresses against the store and hands the
/// results in, and the tests call the same shapes without a database.
///
/// The TypeSafe/Jev method (LAW-347) rides the same [`DecisionLog`] shape:
/// its model id and probabilities fill the fields the address-match method
/// always leaves absent, and the method spelling marks who decided.
pub mod assign_match {
    use serde::Serialize;
    use uuid::Uuid;

    /// The mailbox local parts correspondence floods through — the court
    /// e-filing notice feed and the docket robot. They name no Person and
    /// identify no matter, so a thread whose only candidate carries one
    /// matches nothing. Matched case-insensitively on the local part.
    const SHARED_INBOX_LOCAL_PARTS: &[&str] = &["efiling-notice", "autodocket"];

    /// Whether an address is a shared inbox the match must ignore.
    #[must_use]
    pub fn is_shared_inbox(address: &str) -> bool {
        let local = address
            .trim()
            .split('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        SHARED_INBOX_LOCAL_PARTS.contains(&local.as_str())
    }

    /// One thread address the door resolved against the Person CRM: the
    /// Person it belongs to, if any, and the open Projects that Person
    /// participates in. Entity emails join the same shape when Entities
    /// carry email storage; today no Entity field holds one.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ResolvedAddress {
        pub address: String,
        pub person_id: Option<Uuid>,
        pub open_projects: Vec<Uuid>,
    }

    /// Why an unmatched thread stayed in the inbox.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum LeftReason {
        /// The sender address resolves to no Person row.
        NoSenderPerson,
        /// Every identifying address resolved, but named no open Project.
        NoMatch,
        /// More than one open Project matched; choosing is the lawyer's.
        Ambiguous,
        /// The LAW-346 ZDR gate is closed — no operator flag or no key —
        /// so the Jev method never ran.
        JevGateClosed,
        /// The Jev call failed (auth, status, timeout, unreadable answer);
        /// the thread falls back to the lawyer.
        JevUnavailable,
        /// The Firm's open Projects exceed the wire's 255-option ceiling.
        JevTooManyProjects,
        /// Jev answered but carried no usable probability distribution.
        JevNoProbabilities,
        /// Jev's confidence sat under the proposal band.
        JevBelowThreshold,
        /// Jev chose the `none` option: not matter mail.
        JevNotMatter,
        /// Two Projects sat within a tie's distance of the choice.
        JevTied,
    }

    /// What the assignment method decided for one thread.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
    #[serde(tag = "decision", rename_all = "snake_case")]
    pub enum MatchOutcome {
        /// Exactly one open Project matched; the thread assigns to it.
        Assigned { project_id: Uuid },
        /// Jev proposes the Project in the middle confidence band; the
        /// thread stays in the inbox and the lawyer confirms before
        /// anything assigns.
        Proposed { project_id: Uuid },
        /// The thread stays in the inbox for the lawyer.
        LeftInInbox { reason: LeftReason },
    }

    /// The decision log entry. The address-match method fills the first
    /// fields and leaves the Jev fields `None`; the log's consumers read
    /// one shape across both methods, so the later slice adds its values
    /// without churning the format.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
    pub struct DecisionLog {
        /// The assignment method that decided — `"address-match"` here.
        pub method: &'static str,
        pub message: Uuid,
        /// The Project the thread was assigned to, if any.
        pub project_id: Option<Uuid>,
        /// Whether the thread was left in the inbox for the lawyer.
        pub left_in_inbox: bool,
        /// Why an unmatched thread stayed, when it stayed.
        pub reason: Option<LeftReason>,
        /// The Jev model id — set when the Jev method decided the thread.
        pub model_id: Option<String>,
        /// The Jev probabilities ditto.
        pub probabilities: Option<serde_json::Value>,
    }

    impl MatchOutcome {
        /// The decision log entry this outcome produces.
        #[must_use]
        pub fn decision_log(&self, message: Uuid) -> DecisionLog {
            let (project_id, left_in_inbox, reason) = match self {
                Self::Assigned { project_id } => (Some(*project_id), false, None),
                Self::Proposed { project_id } => (Some(*project_id), true, None),
                Self::LeftInInbox { reason } => (None, true, Some(*reason)),
            };
            DecisionLog {
                method: "address-match",
                message,
                project_id,
                left_in_inbox,
                reason,
                model_id: None,
                probabilities: None,
            }
        }
    }

    /// Decide one thread. The From address is the sender: it must resolve
    /// to a Person row or the thread stays. Every non-shared address then
    /// contributes its open Projects, and exactly one distinct Project
    /// across the whole thread assigns it — zero or several stay.
    #[must_use]
    pub fn decide(from: &str, resolved: &[ResolvedAddress]) -> MatchOutcome {
        let sender = resolved
            .iter()
            .find(|row| row.address.eq_ignore_ascii_case(from.trim()) && !is_shared_inbox(from));
        let Some(sender) = sender else {
            // A shared inbox is non-identifying: it is no match, not a
            // missing sender.
            return MatchOutcome::LeftInInbox {
                reason: if is_shared_inbox(from) {
                    LeftReason::NoMatch
                } else {
                    LeftReason::NoSenderPerson
                },
            };
        };
        if sender.person_id.is_none() {
            return MatchOutcome::LeftInInbox {
                reason: LeftReason::NoSenderPerson,
            };
        }
        let mut projects: Vec<Uuid> = resolved
            .iter()
            .filter(|row| !is_shared_inbox(&row.address) && row.person_id.is_some())
            .flat_map(|row| row.open_projects.iter().copied())
            .collect();
        projects.sort_unstable();
        projects.dedup();
        match projects.as_slice() {
            [project_id] => MatchOutcome::Assigned {
                project_id: *project_id,
            },
            [] => MatchOutcome::LeftInInbox {
                reason: LeftReason::NoMatch,
            },
            _ => MatchOutcome::LeftInInbox {
                reason: LeftReason::Ambiguous,
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{decide, is_shared_inbox, LeftReason, MatchOutcome, ResolvedAddress};
        use uuid::Uuid;

        fn project(n: u128) -> Uuid {
            Uuid::from_u128(n)
        }

        fn resolved(address: &str, person: Option<u128>, projects: &[u128]) -> ResolvedAddress {
            ResolvedAddress {
                address: address.to_owned(),
                person_id: person.map(project),
                open_projects: projects.iter().map(|n| project(*n)).collect(),
            }
        }

        #[test]
        fn shared_inboxes_are_never_identifying() {
            for address in [
                "efiling-notice@courts.example.test",
                "EFILING-NOTICE@COURTS.EXAMPLE.TEST",
                "autodocket@uscourts.example.test",
            ] {
                assert!(is_shared_inbox(address), "{address}");
                let outcome = decide(
                    address,
                    &[
                        resolved(address, Some(1), &[1]),
                        resolved("lawyer@neonlaw.com", Some(2), &[2]),
                    ],
                );
                assert_eq!(
                    outcome,
                    MatchOutcome::LeftInInbox {
                        reason: LeftReason::NoMatch
                    },
                    "a shared sender identifies nothing, even with others resolved"
                );
            }
            assert!(!is_shared_inbox("client@example.com"));
        }

        #[test]
        fn a_sender_with_no_person_row_stays_in_the_inbox() {
            let outcome = decide(
                "stranger@example.com",
                &[
                    resolved("stranger@example.com", None, &[1]),
                    resolved("client@example.com", Some(7), &[1]),
                ],
            );
            assert_eq!(
                outcome,
                MatchOutcome::LeftInInbox {
                    reason: LeftReason::NoSenderPerson
                }
            );
        }

        #[test]
        fn exactly_one_matched_project_assigns_wherever_it_matched() {
            // From identifies nothing (an alias with no participation), but
            // the Cc names the client on one open Project.
            let outcome = decide(
                "assistant@example.com",
                &[
                    resolved("assistant@example.com", Some(3), &[]),
                    resolved("assistant@example.com", Some(3), &[]), // duplicate candidate
                    resolved("client@example.com", Some(7), &[1]),
                ],
            );
            assert_eq!(
                outcome,
                MatchOutcome::Assigned {
                    project_id: project(1)
                }
            );
        }

        #[test]
        fn two_open_projects_stay_for_the_lawyer() {
            let outcome = decide(
                "client@example.com",
                &[resolved("client@example.com", Some(7), &[1, 2])],
            );
            assert_eq!(
                outcome,
                MatchOutcome::LeftInInbox {
                    reason: LeftReason::Ambiguous
                }
            );
        }

        #[test]
        fn two_addresses_naming_the_same_project_collapse_to_one() {
            let outcome = decide(
                "client@example.com",
                &[
                    resolved("client@example.com", Some(7), &[1]),
                    resolved("spouse@example.com", Some(8), &[1]),
                ],
            );
            assert_eq!(
                outcome,
                MatchOutcome::Assigned {
                    project_id: project(1)
                }
            );
        }

        #[test]
        fn every_resolved_address_missing_the_project_stays() {
            let outcome = decide(
                "client@example.com",
                &[resolved("client@example.com", Some(7), &[])],
            );
            assert_eq!(
                outcome,
                MatchOutcome::LeftInInbox {
                    reason: LeftReason::NoMatch
                }
            );
        }

        #[test]
        fn the_log_carries_the_shape_the_jev_slice_fills() {
            let assigned = MatchOutcome::Assigned {
                project_id: project(1),
            }
            .decision_log(Uuid::from_u128(9));
            assert_eq!(assigned.method, "address-match");
            assert_eq!(assigned.project_id, Some(project(1)));
            assert!(!assigned.left_in_inbox);
            assert!(assigned.model_id.is_none());
            assert!(assigned.probabilities.is_none());

            let left = MatchOutcome::LeftInInbox {
                reason: LeftReason::NoMatch,
            }
            .decision_log(Uuid::from_u128(9));
            assert_eq!(left.project_id, None);
            assert!(left.left_in_inbox);
            assert_eq!(left.reason, Some(LeftReason::NoMatch));

            // The later Jev fields ride the same shape without a migration:
            // the JSON keys are already there.
            let json = serde_json::to_value(&left).unwrap();
            assert!(json.get("model_id").is_some());
            assert!(json.get("probabilities").is_some());
        }

        #[test]
        fn a_jev_proposal_stays_in_the_inbox_and_names_the_project() {
            let proposed = MatchOutcome::Proposed {
                project_id: project(1),
            }
            .decision_log(Uuid::from_u128(9));
            assert_eq!(proposed.project_id, Some(project(1)));
            assert!(proposed.left_in_inbox);
            assert_eq!(proposed.reason, None);
        }

        #[test]
        fn the_jev_log_fills_the_reserved_fields_without_changing_their_shape() {
            // The portal's Jev step fills the reserved fields in place —
            // model id, and a free-form value carrying the confidence,
            // the body flag, the chosen option, and the distribution.
            let probabilities = serde_json::json!({
                "chosen": "none",
                "confidence": 0.72,
                "body_used": true,
                "probabilities": { "none": 0.72, "other": 0.28 },
            });
            let mut entry = MatchOutcome::LeftInInbox {
                reason: LeftReason::JevNotMatter,
            }
            .decision_log(Uuid::from_u128(9));
            entry.method = "jev";
            entry.model_id = Some("jev-1.14.0".to_owned());
            entry.probabilities = Some(probabilities.clone());
            assert_eq!(entry.method, "jev");
            assert_eq!(entry.model_id.as_deref(), Some("jev-1.14.0"));
            assert_eq!(entry.probabilities, Some(probabilities));

            // The reserved keys survive the fill without a migration.
            let json = serde_json::to_value(&entry).unwrap();
            assert!(json.get("model_id").is_some());
            assert!(json.get("probabilities").is_some());
        }

        #[test]
        fn the_jev_reasons_spell_out_why_the_model_did_not_decide() {
            // The exact wire spellings — these strings land in the
            // decision log and the operator CLI prints them.
            let spellings = [
                (LeftReason::JevGateClosed, "jev_gate_closed"),
                (LeftReason::JevUnavailable, "jev_unavailable"),
                (LeftReason::JevTooManyProjects, "jev_too_many_projects"),
                (LeftReason::JevNoProbabilities, "jev_no_probabilities"),
                (LeftReason::JevBelowThreshold, "jev_below_threshold"),
                (LeftReason::JevNotMatter, "jev_not_matter"),
                (LeftReason::JevTied, "jev_tied"),
            ];
            for (reason, spelling) in spellings {
                assert_eq!(
                    serde_json::to_value(reason).unwrap(),
                    serde_json::json!(spelling)
                );
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // Independent flags: unread, matter consent, promotional opt-in, promotional opt-out.
pub struct Conversation {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub brand_key: Option<String>,
    pub interaction_type: String,
    pub provider_post_id: Option<String>,
    pub project_id: Option<Uuid>,
    pub channel: String,
    pub external_address: String,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    pub unread: bool,
    pub provider_account: String,
    pub provider_thread_ref: Option<String>,
    pub source_page: Option<String>,
    pub source_channel: Option<String>,
    pub matter_consent: bool,
    pub promotional_opt_in: bool,
    pub promotional_opt_out: bool,
    pub email_conversation_id: Option<Uuid>,
    /// The Person attributed to this conversation. Inbound email resolves
    /// this at ingest; an SMS link records it after the lawyer confirms the
    /// sender's number belongs to the Person.
    pub person_id: Option<Uuid>,
    /// When the conversation was moved to the Inbox trash; `None` while it
    /// is in the Inbox. See [`set_trashed`].
    pub trashed_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub parent_message_id: Option<Uuid>,
    pub visibility: Option<String>,
    pub direction: String,
    pub body: String,
    pub author_person_id: Option<Uuid>,
    pub provider_message_id: Option<String>,
    pub provider_ref: Option<String>,
    pub delivery_state: String,
    pub purpose: String,
    pub attachment_asset_id: Option<Uuid>,
    /// Object-storage key of the verbatim payload this Message was ingested
    /// from — the raw `.eml`, or the member data-export CSV for an imported
    /// LinkedIn thread (LAW-238 D1). Webhook-delivered mail retains its raw
    /// source here independently of optional email threading.
    pub raw_storage_key: Option<String>,
    pub communication_id: Option<Uuid>,
    pub occurred_at: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub firm_id: Uuid,
    pub from_project_id: Option<Uuid>,
    pub to_project_id: Uuid,
    pub actor_person_id: Uuid,
    pub inserted_at: DateTime<Utc>,
}

/// One attachment on an ingested provider message: its content-addressed
/// Asset, original filename, and sha256. The provider's attachment order is
/// preserved on the link; the first attachment is the Message's primary one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestAttachment {
    pub asset_id: Uuid,
    pub filename: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct IngestInbound<'a> {
    pub firm_id: Uuid,
    /// Project attribution set at ingest time (the CLI ingest door). Support
    /// and webhook paths leave it `None`: the conversation stays on the Firm
    /// until an Owner assigns it. Only threads carrying the same attribution
    /// absorb the delivery — an unassigned thread never silently adopts one.
    pub project_id: Option<Uuid>,
    pub brand_key: Option<&'a str>,
    pub interaction_type: Option<&'a str>,
    pub provider_post_id: Option<&'a str>,
    pub parent_provider_message_id: Option<&'a str>,
    pub visibility: Option<&'a str>,
    pub channel: &'a str,
    pub provider_account: &'a str,
    pub provider_message_id: &'a str,
    pub provider_thread_ref: Option<&'a str>,
    pub external_address: &'a str,
    pub external_name: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub body: &'a str,
    pub source_page: Option<&'a str>,
    pub source_channel: Option<&'a str>,
    pub matter_consent: bool,
    pub promotional_opt_in: bool,
    pub email_conversation_id: Option<Uuid>,
    pub attachments: Vec<IngestAttachment>,
    pub occurred_at: &'a str,
    /// Object-storage key of the archived source, including raw email and
    /// import payloads; see [`Message::raw_storage_key`]. Absent when unavailable.
    pub raw_storage_key: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestedInbound {
    pub conversation_id: Uuid,
    pub message_id: Uuid,
    pub deduped: bool,
    /// Whether this delivery wrote the conversation's `person_id` — the
    /// event a Relationship Log entry records exactly once per thread.
    pub person_linked: bool,
}

#[derive(Debug, Clone)]
pub struct OutboundEnvelope {
    pub channel: String,
    pub account: String,
    pub interaction_type: String,
    /// Provider post or media containing the interaction, when available.
    pub provider_post_id: Option<String>,
    /// Provider comment/message that this reply should answer.
    pub reply_to_provider_message_id: Option<String>,
    /// Provider thread or recipient reference used by messaging APIs.
    pub provider_thread_ref: Option<String>,
    pub to: String,
    pub subject: Option<String>,
    pub body: String,
    pub purpose: String,
    pub visibility: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportOutcome {
    Submitted { provider_ref: String },
    Unknown,
    Rejected { detail: String },
}

#[async_trait]
pub trait OutboundTransport: Send + Sync {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchReport {
    pub submitted: usize,
    pub unknown: usize,
    pub suppressed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // Independent list facts: unread, attachments, sent, trashed.
pub struct IntakeRow {
    /// The Person whose recorded phone is this text's number, when known.
    pub contact: Option<ContactLink>,
    pub id: Uuid,
    pub firm_id: Uuid,
    pub brand_key: Option<String>,
    pub project_id: Option<Uuid>,
    pub channel: String,
    pub assignee: String,
    pub unread: bool,
    pub delivery: String,
    pub next_action: String,
    pub external_address: String,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    /// The direction of the conversation's most recent message — what the
    /// `?direction=` filter matches rows on (LAW-238 D3). Empty when the
    /// conversation has no messages yet, so an empty conversation matches
    /// no direction filter.
    pub direction: String,
    /// Whether any message on the conversation carries a filed attachment.
    /// What the `?attachments=true` filter matches rows on.
    pub has_attachments: bool,
    /// Whether a Person on the firm side sent a message on the conversation —
    /// an outbound message with an author. What the inbox's Sent folder
    /// lists; an automated acknowledgement has no author and never counts.
    pub sent: bool,
    /// Whether the conversation is in the Inbox trash. The store lists
    /// trashed conversations too — billing and reporting read every row —
    /// and the Inbox shows them only in its Trash folder.
    pub trashed: bool,
    /// Opening snippet of the conversation's most recent written message
    /// (same `occurred_at, id` order as [`IntakeRow::direction`]), whitespace
    /// collapsed and ellipsized — what a list row renders beneath the
    /// subject. An automated acknowledgement (outbound, no author) is skipped
    /// so the row previews what the counterparty or the firm actually wrote.
    /// None when no such message exists or its body is blank.
    pub preview: Option<String>,
    /// The Person the inbound sender resolved to at ingest, when one was
    /// persisted — display only, as [`IntakeRow::contact`] is.
    pub person: Option<PersonBadge>,
    /// The latest email exchanged with the counterparty, if its date is available.
    pub email_sent_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Filters on a Firm Inbox page. The store applies them before choosing the page.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)] // Independent filters from the Inbox rail.
pub struct IntakeFilters<'a> {
    pub brand: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub direction: Option<&'a str>,
    pub unread_only: bool,
    pub attachments_only: bool,
    pub sent_only: bool,
    pub trash_only: bool,
}

/// The final row's ordering key. IDs make equal activity times deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeCursor {
    pub activity_at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Debug)]
pub struct IntakePage {
    pub rows: Vec<IntakeRow>,
    pub next: Option<IntakeCursor>,
}

/// The Person a text's number belongs to. Display only: a recorded phone is
/// an address, never proof of who is texting, so it grants nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactLink {
    pub person_id: Uuid,
    pub name: String,
    /// The Person's role, lowercased (`client`, `lawyer`, ...).
    pub role: String,
}

impl ContactLink {
    pub(crate) fn of(person: &persons::Person) -> Self {
        Self {
            person_id: person.id,
            name: person.name.clone(),
            role: person.role.as_str().to_string(),
        }
    }
}

/// The Person an inbound conversation's sender resolved to at ingest — the
/// association persisted on the conversation (`conversation.person_id`),
/// surfaced display-only. A resolved identity is provenance, not authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonBadge {
    pub person_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectChoice {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    /// The matter's own description, when it carries one — the Jev option
    /// generator's raw material, kept beside the name it renders with.
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntakeDetail {
    pub contact: Option<ContactLink>,
    pub conversation: Conversation,
    pub messages: Vec<Message>,
    pub bodies_visible: bool,
    pub can_social_reply: bool,
    pub can_sms_reply: bool,
    pub projects: Vec<ProjectChoice>,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadView {
    pub conversation: Conversation,
    pub messages: Vec<Message>,
    pub assignee: String,
    pub delivery: String,
    pub next_action: String,
    pub can_note: bool,
    pub can_send: bool,
    pub can_attribute: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("database: {0}")]
    Db(String),
    #[error("not allowed")]
    Denied,
    #[error("conversation was not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("that project is in another firm")]
    CrossFirm,
    #[error("empty message")]
    Empty,
    #[error("promotional send is opted out")]
    OptedOut,
    /// The import entry point requires the postgres Inbox authority —
    /// imports are postgres-native (LAW-238 D1) and the legacy Surreal path
    /// has no direction-aware writer.
    #[error("inbox import requires PostgreSQL (set NAVIGATOR_POSTGRES_HOST)")]
    PostgresAuthorityRequired,
    #[error("{0}")]
    Contact(String),
    #[error("unknown inbox route: {0}")]
    InvalidRoute(String),
}

impl From<persons::PersonError> for InboxError {
    fn from(error: persons::PersonError) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<communications::CommunicationError> for InboxError {
    fn from(error: communications::CommunicationError) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<crate::email_conversations::EmailConversationError> for InboxError {
    fn from(error: crate::email_conversations::EmailConversationError) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<ProjectStoreError> for InboxError {
    fn from(error: ProjectStoreError) -> Self {
        Self::Db(error.to_string())
    }
}

/// The postgres pool when the deployment has cut the Inbox slice over.
/// Configuration errors fail closed rather than silently keeping Surreal.
pub(crate) async fn inbox_authority() -> Result<crate::postgres::PostgresDb, InboxError> {
    crate::postgres::inbox::authority()
        .await
        .map_err(|error| InboxError::Db(error.to_string()))
}

pub(crate) fn allows_external_reply(conversation: &Conversation) -> bool {
    conversation.channel != channel::VOICE
        && !crate::fi_import::is_account(&conversation.provider_account)
}

/// The label the inbox shows for what the viewer should do next.
#[must_use]
pub fn next_action(
    role: Role,
    assigned: bool,
    delivery_state: &str,
    can_send: bool,
) -> &'static str {
    if role == Role::Owner && !assigned {
        return "Assign to a project";
    }
    if role == Role::Owner {
        return "Reassign";
    }
    if can_send && delivery_state == delivery::UNKNOWN {
        return "Reconcile delivery";
    }
    if can_send {
        return "Send a reply";
    }
    if matches!(role, Role::Lawyer | Role::Clerk) {
        return "Leave an internal note";
    }
    "Review"
}

/// Ingest clean provider attachment bytes after the receiving Firm is resolved.
/// A bare Asset UUID or supplied hash cannot mint Firm staging authority.
pub async fn ingest_attachment(
    db: &ApplicationStore,
    storage: &Arc<dyn cloud::StorageService>,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<IngestAttachment, InboxError> {
    let pg = inbox_authority().await?;
    ingest_attachment_on(
        Some(&pg),
        db,
        storage,
        firm_id,
        project_id,
        filename,
        content_type,
        bytes,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ingest_attachment_on(
    pg: Option<&crate::postgres::PostgresDb>,
    db: &ApplicationStore,
    storage: &Arc<dyn cloud::StorageService>,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<IngestAttachment, InboxError> {
    let sha256 = crate::assets::sha256_hex(bytes);
    let asset_id = if let Some(project_id) = project_id {
        let project = projects::find_by_id(db, project_id)
            .await?
            .ok_or(InboxError::NotFound)?;
        if project.firm_id != Some(firm_id) {
            return Err(InboxError::CrossFirm);
        }
        crate::documents::ingest_bytes(
            db,
            storage,
            &crate::documents::IngestArgs {
                project_id,
                source: crate::documents::source::EMAIL,
                filename,
                kind: "unclassified",
                content_type,
                description: None,
                secondary_storage_key: None,
                visibility: "internal",
            },
            bytes,
        )
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?
        .asset_id
    } else {
        crate::assets::ingest_content(db, storage, bytes, content_type)
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?
    };
    if let Some(pg) = pg {
        if project_id.is_none() {
            crate::postgres::inbox::record_attachment_receipt_on(pg, firm_id, asset_id, &sha256)
                .await?;
        }
    }
    Ok(IngestAttachment {
        asset_id,
        filename: filename.into(),
        sha256,
    })
}

/// Ingest one provider delivery. A repeated webhook with the same Firm,
/// channel, account, and provider message id returns the original row.
///
/// # Errors
///
/// Propagates any database error.
pub async fn ingest_inbound(
    db: &ApplicationStore,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    Box::pin(ingest_inbound_in(db, inbound, None)).await
}

/// Ingest one provider email into the Conversation its thread already
/// occupies. Before the per-sender lookup [`ingest_inbound`] runs, the
/// delivery joins an existing Conversation of the same Firm, channel, and
/// Project attribution when either
///
/// - `inbound.provider_thread_ref` (a provider thread id such as Gmail's
///   `X-GM-THRID`) is that Conversation's thread, or
/// - one of `references` — RFC 5322 Message-IDs from `In-Reply-To` and
///   `References`, nearest ancestor first, without angle brackets — is the
///   `provider_message_id` of a Message already in it.
///
/// So an inbound message and the Firm's reply to it land in one
/// Conversation whatever their senders. A Conversation with a different
/// Project attribution is never joined. Dedup on the provider message id
/// runs first, unchanged.
///
/// # Errors
///
/// Propagates any database error.
pub async fn ingest_inbound_threaded(
    db: &ApplicationStore,
    inbound: &IngestInbound<'_>,
    references: &[String],
) -> Result<IngestedInbound, InboxError> {
    Box::pin(ingest_inbound_in(db, inbound, Some(references))).await
}

/// `thread` is `None` for the per-sender threading of [`ingest_inbound`]
/// and the reference chain for [`ingest_inbound_threaded`].
async fn ingest_inbound_in(
    db: &ApplicationStore,
    inbound: &IngestInbound<'_>,
    thread: Option<&[String]>,
) -> Result<IngestedInbound, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::ingest_threaded_on(
        &pg, db, inbound, thread,
    ))
    .await
}

/// The Conversation the nearest referenced Message lives in: `references`
/// is ordered nearest ancestor first, and `hits` pairs each referenced
/// provider message id found in scope with its Conversation.
pub(crate) fn nearest_referenced_conversation(
    references: &[String],
    hits: &[(String, Uuid)],
) -> Option<Uuid> {
    references.iter().find_map(|reference| {
        hits.iter()
            .find(|(provider_message_id, _)| provider_message_id == reference)
            .map(|(_, conversation_id)| *conversation_id)
    })
}

/// The direction-aware ingest entry for imported correspondence (LAW-238
/// D1): lawyer-sent items arrive as `outbound` with the mailbox owner as
/// author, received mail as `inbound`. Dedup, threading, and projection are
/// the same machinery [`ingest_inbound`] runs.
///
/// Imports are postgres-native, so this entry requires the postgres Inbox
/// authority — the legacy Surreal store has no direction-aware writer, and
/// writing one would be new Surreal-bound code, which the removal forbids.
///
/// # Errors
///
/// Propagates any database error, and
/// [`InboxError::PostgresAuthorityRequired`] when the deployment has not cut
/// the Inbox slice over to postgres.
pub async fn ingest_imported(
    direction_value: &str,
    author_person_id: Option<Uuid>,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::ingest_message_on(&pg, direction_value, author_person_id, None, inbound)
        .await
}

/// Point an imported Message at its first filed attachment Asset (LAW-238
/// D1) — the postgres-authoritative counterpart of
/// [`crate::postgres::inbox::set_attachment_asset_on`]. No-op when the slot
/// is already set.
///
/// # Errors
///
/// Propagates any database error, and
/// [`InboxError::PostgresAuthorityRequired`] when the deployment has not cut
/// the Inbox slice over to postgres.
pub async fn set_attachment_asset(message_id: Uuid, asset_id: Uuid) -> Result<(), InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::set_attachment_asset_on(&pg, message_id, asset_id).await
}

/// The guard every email association runs through: a lawyer- or clerk-tier
/// resolved Person is that firm member's outgoing mail and never the
/// counterparty of inbound correspondence. The portal's lawyer-mailbox
/// guard refuses such mail before ingest; this is the store-level backstop
/// for the doors that bypass it (CLI ingest, imports).
pub(crate) fn counterparty_decision(person: &persons::Person) -> Option<Uuid> {
    if person.role.is_lawyer_tier() || person.role.is_clerk() {
        None
    } else {
        Some(person.id)
    }
}

/// The Relationship Log action an inbound association records: identity
/// provenance on the lawyer-acknowledged feed — automation never writes
/// typed `relationship` or `entity_role` edges from correspondence.
pub(crate) const INBOUND_ASSOCIATION_ACTION: &str = "inbound_email_associated";

/// The Person name an auto-created sender takes: the RFC From display name
/// when present, the address itself otherwise.
pub(crate) fn sender_display_name<'a>(display: Option<&'a str>, address: &'a str) -> &'a str {
    display
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| address.trim())
}

pub(crate) fn association_log(
    conversation_id: Uuid,
    person_id: Uuid,
    channel: &str,
    address: &str,
) -> crate::relationship_logs::NewRelationshipLog {
    crate::relationship_logs::NewRelationshipLog {
        actor_person_id: None,
        subject_type: crate::relationship_logs::PERSON_SUBJECT.to_string(),
        subject_id: person_id,
        action: INBOUND_ASSOCIATION_ACTION.to_string(),
        detail: format!("inbox_conversation:{conversation_id} {channel} sender {address}"),
        project_id: None,
    }
}

/// Append a best-effort Relationship Log entry for Source ingestion or
/// explicit backfill. Native PostgreSQL ingestion appends the same entry in
/// its transaction. An identical replay is skipped; `person_id` is None
/// when no link was newly made (guard rejection, non-email channel, replay).
pub(crate) async fn record_association_log(
    db: &ApplicationStore,
    conversation_id: Uuid,
    person_id: Option<Uuid>,
    channel: &str,
    address: &str,
) {
    let Some(person_id) = person_id else {
        return;
    };
    let entry = association_log(conversation_id, person_id, channel, address);
    match crate::relationship_logs::exists(db, &entry).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = crate::relationship_logs::record(db, &entry).await {
                tracing::warn!(
                    %error,
                    %conversation_id,
                    "relationship log entry for the inbox association stalled"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                %conversation_id,
                "relationship log dedup check stalled"
            );
        }
    }
}

/// Links still-unassociated email conversations to the Person the address
/// resolves to today — the same resolve-or-create the ingest uses — and
/// reports how many conversations gained a link.
///
/// Idempotent: only rows with `person_id IS NONE` are selected, so a
/// re-run finds nothing and writes nothing. Associations the backfill
/// makes behave like ingest associations: guarded against lawyer- and
/// clerk-tier senders, logged once on the Relationship Log, never edges.
///
/// # Errors
///
/// Propagates any database error, including the fail-closed authority
/// probe error.
pub async fn backfill_person_links(db: &ApplicationStore) -> Result<u64, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::backfill_person_links_on(&pg, db)).await
}

/// The attachments linked to one Inbox Message, in provider order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageAttachment {
    pub asset_id: Uuid,
    pub filename: String,
    pub position: i64,
    pub sha256: String,
}

/// Link a Message to its attachment Assets in provider order. Re-linking
/// the same message and sha reuses the row, so a retried ingest cannot
/// duplicate a link.
///
/// # Errors
///
/// Propagates database and Inbox authority errors.
pub async fn link_message_assets(
    _db: &ApplicationStore,
    firm_id: Uuid,
    message_id: Uuid,
    attachments: &[IngestAttachment],
) -> Result<(), InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::link_message_assets_on(&pg, firm_id, message_id, attachments).await
}

/// The attachments linked to one Inbox Message, in provider order. Firm
/// scoping rides on the link row: a caller outside the message's Firm
/// sees nothing.
///
/// # Errors
///
/// Propagates any database error.
pub async fn message_assets(
    _db: &ApplicationStore,
    firm_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<MessageAttachment>, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::message_assets_on(&pg, firm_id, message_id).await
}

/// Check the authoritative Person and participation before an Inbox assignment.
/// Owners may assign across Projects; Lawyers and Clerks require participation.
///
/// # Errors
/// Returns an error when the Person or participation cannot be read.
pub async fn can_assign_project(
    db: &ApplicationStore,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    let actor = load_person(db, actor_id).await?;
    match actor.role {
        Role::Owner => Ok(true),
        Role::Lawyer | Role::Clerk => sees_project(db, actor_id, actor.role, project_id).await,
        Role::Admin | Role::Client => Ok(false),
    }
}

/// Attribute or reassign a conversation to a Project in the same Firm.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor is not an Owner or a participating Lawyer/Clerk.
/// [`InboxError::CrossFirm`] when the Project belongs to another Firm.
/// [`InboxError::NotFound`] when the conversation or Project is missing.
/// Voice billing and read-only Fi SMS history leave the Firm queue once.
/// Repeated assignment to that Project repairs a checkpoint gap without
/// allowing a concurrent import to move the history to another Project.
pub async fn assign(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    // The assign door only opens open Projects: a stale form or a crafted
    // POST cannot attribute a conversation into a closed or archived matter.
    let target = projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !target.status.eq_ignore_ascii_case("open") {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel == channel::VOICE
        || (conversation.channel == channel::SMS
            && crate::fi_import::is_account(&conversation.provider_account))
    {
        let pg = crate::postgres::PostgresDb::application()
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        return Box::pin(crate::billed_calls::assign_conversation(
            &pg,
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await;
    }
    assign_unbilled(db, actor_id, conversation_id, project_id).await
}

/// Require ordinary Inbox assignment rights using the authoritative Person and Project.
/// Owner may assign; a Lawyer or Clerk needs target Project participation.
/// The target Project must belong to the Conversation or import's resolved Firm.
pub async fn require_assignment_authorization(
    db: &ApplicationStore,
    actor_id: Uuid,
    firm_id: Uuid,
    project_id: Uuid,
) -> Result<(), InboxError> {
    let project = projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(firm_id) {
        return Err(InboxError::CrossFirm);
    }
    if !can_assign_project(db, actor_id, project_id).await? {
        return Err(InboxError::Denied);
    }
    Ok(())
}

pub(crate) async fn assign_unbilled(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::assign_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        project_id,
    ))
    .await
}

/// The triage review queue's assignment door. Owner may act across Firms;
/// Admin requires membership in the target Firm. A Lawyer or Clerk requires
/// participation on the Project the suggestion or correction names.
///
/// # Errors
///
/// [`InboxError::Denied`] for a Client, an actor outside the Firm, or a
/// Lawyer/Clerk without active participation on the Project. The other
/// cases of [`assign`] apply unchanged.
pub(crate) async fn assign_for_triage_review(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel == channel::VOICE
        || (conversation.channel == channel::SMS
            && crate::fi_import::is_account(&conversation.provider_account))
    {
        let pg = crate::postgres::PostgresDb::application()
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        return Box::pin(crate::billed_calls::assign_conversation_for_triage_review(
            &pg,
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await;
    }
    Box::pin(assign_for_triage_review_unbilled(
        db,
        actor_id,
        conversation_id,
        project_id,
    ))
    .await
}

pub(crate) async fn assign_for_triage_review_unbilled(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::assign_for_triage_review_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        project_id,
    ))
    .await
}

/// Whether the actor may act on a triage row whose human decision assigns
/// `project_id`. Owner always, Admin within their Firm; a Lawyer or Clerk
/// passes the participation check the reply doors use; a Client never.
pub(crate) async fn triage_reviewer_may_assign(
    db: &ApplicationStore,
    actor: &persons::Person,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    match actor.role {
        Role::Owner => Ok(true),
        Role::Admin => {
            let Some(project) = projects::find_by_id(db, project_id).await? else {
                return Ok(false);
            };
            match project.firm_id {
                Some(firm_id) => triage_reviewer_may_resolve(db, actor, firm_id).await,
                None => Ok(false),
            }
        }
        Role::Lawyer | Role::Clerk => sees_project(db, actor.id, actor.role, project_id).await,
        Role::Client => Ok(false),
    }
}

/// Whether the actor may act on a triage row that names no Project —
/// privilege resolution and re-runs. Owner always; Admin, Lawyer, or
/// Clerk when they belong to the row's Firm (the same visibility the queue
/// list grants); a Client never.
pub(crate) async fn triage_reviewer_may_resolve(
    db: &ApplicationStore,
    actor: &persons::Person,
    firm_id: Uuid,
) -> Result<bool, InboxError> {
    match actor.role {
        Role::Owner => Ok(true),
        Role::Admin | Role::Lawyer | Role::Clerk => {
            Ok(crate::firms::memberships_for_person(db, actor.id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .iter()
                .any(|membership| membership.firm_id == firm_id))
        }
        Role::Client => Ok(false),
    }
}

/// Authorization-only door for the triage review actions: denies before any
/// row or suggestion state is read, so an unauthorized actor learns nothing
/// from the error it receives.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor fails the review authority,
/// [`InboxError::NotFound`] when the actor does not exist.
pub(crate) async fn require_triage_reviewer(
    db: &ApplicationStore,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<(), InboxError> {
    let actor = load_person(db, actor_id).await?;
    if triage_reviewer_may_assign(db, &actor, project_id).await? {
        Ok(())
    } else {
        Err(InboxError::Denied)
    }
}

/// Post-operation that attributes an intake message to one Project after it
/// has been received. Only an Owner or a participating Lawyer/Clerk may do so,
/// and only into an open Project — the door refuses closed and archived
/// matters with [`InboxError::Denied`].
pub async fn attribute_to_project(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    assign(db, actor_id, conversation_id, project_id).await
}

/// Participating Lawyer or Clerk leaves a note that never leaves the firm.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot see the assigned Project or
/// is not a Lawyer or Clerk. [`InboxError::Empty`] when the body is blank.
pub async fn post_note(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::post_note_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        body,
    ))
    .await
}

/// Queue an external reply. Only a participating Lawyer or Clerk may queue one.
///
/// # Errors
///
/// [`InboxError::Denied`] for every other role, including Owner and Admin,
/// and for a Lawyer or Clerk who does not currently participate.
/// [`InboxError::OptedOut`] when the reply is promotional and the address
/// has opted out.
pub async fn queue_reply(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
    purpose_value: &str,
) -> Result<Uuid, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::queue_reply_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        body,
        purpose_value,
    ))
    .await
}

/// Queue a social reply with an explicit public/private audience. The system
/// Owner and the Brand's Firm Admin DRI may manage social engagement;
/// Lawyers and Clerks must participate on the assigned Project. This path is
/// separate from matter email/SMS dispatch authorization.
pub async fn queue_social_reply(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
    visibility: &str,
) -> Result<Uuid, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::queue_social_reply_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        body,
        visibility,
    ))
    .await
}

/// The `ExternalSystem` a social channel's sender ids live under.
fn social_system(channel_name: &str) -> Option<external_identities::ExternalSystem> {
    match channel_name {
        channel::FACEBOOK => Some(external_identities::ExternalSystem::Facebook),
        channel::INSTAGRAM => Some(external_identities::ExternalSystem::Instagram),
        channel::THREADS => Some(external_identities::ExternalSystem::Threads),
        channel::YOUTUBE => Some(external_identities::ExternalSystem::Youtube),
        channel::TIKTOK => Some(external_identities::ExternalSystem::Tiktok),
        _ => None,
    }
}

/// Who a conversation's sender is — an address-book lookup, display only.
/// Email resolves by the address a Person records, SMS by the phone a
/// Person records, and the social channels through the identity address
/// book keyed on the provider id the inbound webhook recorded — the same
/// value `Conversation.external_address` carries.
///
/// The resolved [`ContactLink`] grants nothing: it never satisfies a
/// dispatch, participation, or privilege check — those read the person's
/// role and participation, never this table.
///
/// # Errors
///
/// Propagates any database error.
pub(crate) async fn sender_identity(
    db: &ApplicationStore,
    conversation: &Conversation,
) -> Result<Option<ContactLink>, InboxError> {
    match conversation.channel.as_str() {
        channel::EMAIL => Ok(match conversation.person_id {
            // The association persisted at ingest answers first; the
            // address read below only repairs rows that predate it.
            Some(person_id) => persons::find_by_id(db, person_id)
                .await?
                .as_ref()
                .map(ContactLink::of),
            None => persons::find_by_any_email(db, conversation.external_address.trim())
                .await?
                .as_ref()
                .map(ContactLink::of),
        }),
        phone if has_phone_counterparty(phone) => {
            Ok(persons::find_by_phone(db, &conversation.external_address)
                .await?
                .as_ref()
                .map(ContactLink::of))
        }
        other => match social_system(other) {
            Some(system) => Ok(external_identities::find_person_by_account(
                db,
                system,
                &conversation.external_address,
            )
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?
            .as_ref()
            .map(ContactLink::of)),
            None => Ok(None),
        },
    }
}

/// Record that this text conversation's number belongs to the Person with
/// `email`, so later texts resolve to them and the confirmed number reaches
/// their structured contact details. The same people who may reply to a text
/// may link it. A number held by another Person is refused; existing numbers
/// on the selected Person remain intact.
///
/// # Errors
///
/// [`InboxError::Denied`] for a non-SMS conversation or a missing authority;
/// [`InboxError::Contact`] when no Person has that email, the number belongs
/// to someone else, or the Person already records another phone.
pub async fn link_contact_phone(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    email: &str,
) -> Result<ContactLink, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::link_contact_phone_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        email,
    ))
    .await
}

/// Maximum length Twilio accepts for one text request.
pub(crate) const SMS_BODY_LIMIT: usize = 1600;

/// A text starts only toward an E.164 number.
pub(crate) fn is_e164(number: &str) -> bool {
    number.strip_prefix('+').is_some_and(|digits| {
        (8..=15).contains(&digits.len())
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && !digits.starts_with('0')
    })
}

/// Queue a text in an existing SMS conversation from the admin inbox. The
/// Owner and the Firm's Brand-content Admin may reply; a Lawyer or Clerk only
/// while participating on the assigned Project.
///
/// # Errors
///
/// [`InboxError::Denied`] when the conversation is not an SMS thread or the
/// actor lacks authority. [`InboxError::Empty`] for a blank or over-long body.
pub async fn queue_sms_reply(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::queue_sms_reply_on(
        &pg,
        db,
        actor_id,
        conversation_id,
        body,
    ))
    .await
}

/// Open a text conversation with a number that has not texted the Firm first.
/// The sender attests that the recipient agreed to receive texts, which is
/// recorded as matter consent on the conversation. An existing thread with the
/// same number is reused.
///
/// # Errors
///
/// [`InboxError::Denied`] when the number is not E.164, consent was not
/// attested, or the actor lacks authority. [`InboxError::Empty`] for a blank
/// or over-long body.
#[allow(clippy::too_many_arguments)]
pub async fn open_sms_conversation(
    db: &ApplicationStore,
    actor_id: Uuid,
    firm_id: Uuid,
    brand_key: Option<&str>,
    provider_account: &str,
    to: &str,
    body: &str,
    recipient_consented: bool,
) -> Result<(Uuid, Uuid), InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::open_sms_conversation_on(
        &pg,
        db,
        actor_id,
        firm_id,
        brand_key,
        provider_account,
        to,
        body,
        recipient_consented,
    ))
    .await
}

/// Send every queued reply, re-checking Lawyer or Clerk participation first.
///
/// An unknown provider result stays unknown. A later pass does not transmit
/// it again.
///
/// # Errors
///
/// Propagates any database error. A single transport failure is recorded on
/// that message and does not abort the rest of the queue.
pub async fn dispatch_queued(
    db: &ApplicationStore,
    transport: &dyn OutboundTransport,
) -> Result<DispatchReport, InboxError> {
    dispatch_queued_messages(db, transport, None).await
}

/// Dispatch a single queued Message, leaving other queued Messages untouched.
///
/// # Errors
///
/// Propagates database failures while checking and recording delivery.
pub async fn dispatch_queued_message(
    db: &ApplicationStore,
    transport: &dyn OutboundTransport,
    message_id: Uuid,
) -> Result<DispatchReport, InboxError> {
    dispatch_queued_messages(db, transport, Some(message_id)).await
}

async fn dispatch_queued_messages(
    db: &ApplicationStore,
    transport: &dyn OutboundTransport,
    message_id: Option<Uuid>,
) -> Result<DispatchReport, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::dispatch_queued_on(
        &pg, db, transport, message_id,
    ))
    .await
}

pub(crate) fn outbound_envelope(
    conversation: &Conversation,
    message: &Message,
    messages: &[Message],
) -> OutboundEnvelope {
    let reply_to_provider_message_id = messages
        .iter()
        .find(|candidate| {
            candidate.direction == direction::INBOUND && candidate.provider_message_id.is_some()
        })
        .and_then(|candidate| candidate.provider_message_id.clone());
    OutboundEnvelope {
        channel: conversation.channel.clone(),
        account: conversation.provider_account.clone(),
        interaction_type: conversation.interaction_type.clone(),
        provider_post_id: conversation.provider_post_id.clone(),
        reply_to_provider_message_id,
        provider_thread_ref: conversation.provider_thread_ref.clone(),
        to: conversation.external_address.clone(),
        subject: conversation.subject.clone(),
        body: message.body.clone(),
        purpose: message.purpose.clone(),
        visibility: message.visibility.clone(),
    }
}

/// Apply a provider delivery callback scoped by Firm, channel, and account.
///
/// An unknown provider reference records nothing and does not send. A
/// callback that would move delivery backward is ignored. `unknown` may
/// advance to `delivered`.
///
/// # Errors
///
/// Propagates any database error.
pub async fn apply_delivery(
    db: &ApplicationStore,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_ref: &str,
    incoming: &str,
) -> Result<Option<Uuid>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::apply_delivery_on(
        &pg,
        db,
        firm_id,
        channel_name,
        provider_account,
        provider_ref,
        incoming,
    ))
    .await
}

/// Honor a promotional opt-out and suppress queued promotional sends.
///
/// # Errors
///
/// Propagates any database error.
pub async fn record_promotional_opt_out(
    _db: &ApplicationStore,
    firm_id: Uuid,
    channel_name: &str,
    external_address: &str,
) -> Result<usize, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::record_promotional_opt_out_on(
        &pg,
        firm_id,
        channel_name,
        external_address,
    ))
    .await
}

/// Move a conversation to the Inbox trash, or restore it. Trash hides the
/// conversation from the Inbox lists and the Project unread count; it removes
/// nothing, and a new inbound message restores the conversation.
///
/// # Errors
///
/// [`InboxError::Denied`] unless the actor is an Owner, or a Lawyer or Clerk
/// in the conversation's Firm. [`InboxError::NotFound`] when the conversation
/// is missing.
pub async fn set_trashed(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    trashed: bool,
) -> Result<(), InboxError> {
    let actor = load_person(db, actor_id).await?;
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let allowed = match actor.role {
        Role::Owner => true,
        Role::Lawyer | Role::Clerk => crate::firms::memberships_for_person(db, actor_id)
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?
            .iter()
            .any(|membership| membership.firm_id == conversation.firm_id),
        Role::Admin | Role::Client => false,
    };
    if !allowed {
        return Err(InboxError::Denied);
    }
    let pg = inbox_authority().await?;
    crate::postgres::inbox::set_trashed_on(&pg, conversation_id, trashed).await
}

/// Firm intake list. Message bodies are not included.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot access any Firm inbox.
pub async fn list_intake(
    db: &ApplicationStore,
    actor_id: Uuid,
) -> Result<Vec<IntakeRow>, InboxError> {
    intake_rows(db, actor_id, None).await
}

/// The Firm intake list narrowed to one Brand, in the store: the same
/// Firm-scoped rule as [`list_intake`] decides what the actor may see, and
/// the Brand narrowing happens beside it so a caller cannot widen the view.
/// Message bodies are not included.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot access any Firm inbox.
pub async fn list_intake_in_brand(
    db: &ApplicationStore,
    actor_id: Uuid,
    brand_key: &str,
) -> Result<Vec<IntakeRow>, InboxError> {
    intake_rows(db, actor_id, Some(brand_key)).await
}

/// One Firm Inbox page in descending activity order.
pub async fn intake_page(
    db: &ApplicationStore,
    actor_id: Uuid,
    filters: IntakeFilters<'_>,
    after: Option<IntakeCursor>,
    page_size: u16,
) -> Result<IntakePage, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::intake_page_on(&pg, db, actor_id, filters, after, page_size).await
}

/// The Brand keys across the conversations this actor may see, sorted and
/// deduplicated — the filter vocabulary an inbox renders. A Brand with no
/// conversations this actor may see does not appear.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot access any Firm inbox.
pub async fn intake_brands(
    db: &ApplicationStore,
    actor_id: Uuid,
) -> Result<Vec<String>, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::intake_brands_on(&pg, db, actor_id).await
}

async fn intake_rows(
    db: &ApplicationStore,
    actor_id: Uuid,
    brand: Option<&str>,
) -> Result<Vec<IntakeRow>, InboxError> {
    let pg = inbox_authority().await?;
    let rows = crate::postgres::inbox::list_intake_on(&pg, db, actor_id).await?;
    Ok(rows
        .into_iter()
        .filter(|row| brand.is_none_or(|brand| row.brand_key.as_deref() == Some(brand)))
        .collect())
}

/// One unprocessed Firm-inbox message in the process backlog. The body is
/// included for the routing layer's classification; doors never return it
/// to a client. The conversation's subject and addresses give the plan its
/// context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnprocessedRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Option<Uuid>,
    pub channel: String,
    pub provider_message_id: Option<String>,
    pub external_address: String,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub occurred_at: String,
    pub inserted_at: DateTime<Utc>,
}

/// A message's recorded process state. `processed_at: None` means the
/// message has never been processed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessState {
    /// The recorded route spelling (`route::*`).
    pub route: Option<String>,
    /// When the process run recorded it.
    pub processed_at: Option<DateTime<Utc>>,
    /// The recorded NEF-pipeline outcome (`nef_outcome::*`), when the route
    /// ran the pipeline.
    pub nef_outcome: Option<String>,
}

/// The configured mailbox boundary for the scheduled inbox preview.
pub struct MailboxPreviewScope {
    pub firm_id: Uuid,
    pub brand_key: String,
    pub provider_account: String,
    pub(crate) includes_unbranded_intake: bool,
}

impl MailboxPreviewScope {
    /// Construct an approved preview scope. The last argument is the trusted
    /// lawyer intake's exact Firm and canonical provider account, when configured.
    #[must_use]
    pub fn new(
        firm_id: Uuid,
        brand_key: String,
        provider_account: String,
        lawyer_intake: Option<(Uuid, &str)>,
    ) -> Self {
        Self {
            firm_id,
            brand_key,
            includes_unbranded_intake: lawyer_intake == Some((firm_id, provider_account.as_str())),
            provider_account,
        }
    }

    /// Read one page from this mailbox using the PostgreSQL authority.
    ///
    /// # Errors
    ///
    /// Returns an error when PostgreSQL is unavailable or the query fails.
    pub async fn unprocessed(
        &self,
        actor_id: Uuid,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> Result<Vec<UnprocessedRow>, InboxError> {
        let pg = inbox_authority().await?;
        let page = crate::postgres::inbox::MailboxPreviewPage {
            actor_id,
            firm_id: self.firm_id,
            brand_key: &self.brand_key,
            provider_account: &self.provider_account,
            includes_unbranded_intake: self.includes_unbranded_intake,
            page: crate::postgres::inbox::AssignmentPreviewPage { after, limit },
        };
        crate::postgres::inbox::unprocessed_inbox_scoped_on(&pg, None, Some(page), None).await
    }
}

/// The Firm-inbox messages no process run has recorded yet, oldest first.
/// The owner sees every firm's backlog; lawyers, clerks, and admins see
/// their firms'; anyone else is denied. Each row carries the conversation
/// context and the raw body the classification layer reads.
///
/// # Errors
///
/// A store failure or a denial for a client-tier actor.
pub async fn unprocessed_inbox(
    db: &ApplicationStore,
    actor_id: Uuid,
) -> Result<Vec<UnprocessedRow>, InboxError> {
    read_unprocessed_inbox(db, actor_id, None).await
}

type AssignmentCursor = (DateTime<Utc>, Uuid);

/// Read one interactive assignment-preview page of unassigned inbound email.
///
/// # Errors
/// A store failure or a denial for a client-tier actor.
pub async fn unprocessed_assignment_page(
    db: &ApplicationStore,
    actor_id: Uuid,
    after: Option<AssignmentCursor>,
    limit: u16,
) -> Result<Vec<UnprocessedRow>, InboxError> {
    read_unprocessed_inbox(db, actor_id, Some((after, limit))).await
}

async fn read_unprocessed_inbox(
    db: &ApplicationStore,
    actor_id: Uuid,
    preview: Option<(Option<AssignmentCursor>, u16)>,
) -> Result<Vec<UnprocessedRow>, InboxError> {
    let actor = load_person(db, actor_id).await?;
    let visible_firms = if actor.role == Role::Owner {
        None
    } else if matches!(actor.role, Role::Lawyer | Role::Clerk | Role::Admin) {
        Some(
            crate::firms::memberships_for_person(db, actor_id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .into_iter()
                .map(|membership| membership.firm_id)
                .collect::<Vec<_>>(),
        )
    } else {
        return Err(InboxError::Denied);
    };

    let pg = inbox_authority().await?;
    let Some((after, limit)) = preview else {
        return crate::postgres::inbox::unprocessed_inbox_on(&pg, visible_firms.as_deref()).await;
    };
    let page = crate::postgres::inbox::AssignmentPreviewPage {
        after,
        limit: i64::from(limit),
    };
    crate::postgres::inbox::unprocessed_inbox_scoped_on(
        &pg,
        visible_firms.as_deref(),
        None,
        Some(page),
    )
    .await
}

/// Resolve the distinct addresses on an admitted assignment preview page.
/// Interactive previews match open Projects in the receiving Firm. Scheduled
/// previews also require the configured Brand.
///
/// # Errors
/// Returns a store error, or a denial when a scheduled input crosses its Firm.
pub async fn assignment_candidates(
    _db: &ApplicationStore,
    addresses: Vec<(Uuid, String)>,
    scope: Option<&MailboxPreviewScope>,
) -> Result<HashMap<(Uuid, String), assign_match::ResolvedAddress>, InboxError> {
    let addresses: BTreeSet<_> = addresses.into_iter().collect();
    if addresses.is_empty() {
        return Ok(HashMap::new());
    }
    if scope.is_some_and(|scope| {
        addresses
            .iter()
            .any(|(firm_id, _)| *firm_id != scope.firm_id)
    }) {
        return Err(InboxError::Denied);
    }
    let pg = inbox_authority().await?;
    let addresses: Vec<_> = addresses.into_iter().collect();
    let brand = scope.map(|scope| scope.brand_key.as_str());
    Ok(
        crate::postgres::inbox::assignment_candidates_on(&pg, &addresses, brand)
            .await?
            .into_iter()
            .map(|(firm_id, resolved)| ((firm_id, resolved.address.clone()), resolved))
            .collect(),
    )
}

/// The recorded process state of one message. `Ok(None)` means the message
/// does not exist; a state with `processed_at: None` has never been
/// processed.
///
/// # Errors
///
/// A store failure.
pub async fn process_state(
    _db: &ApplicationStore,
    message_id: Uuid,
) -> Result<Option<ProcessState>, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::process_state_on(&pg, message_id).await
}

/// Record the route, processing time, and — when the route ran the NEF
/// pipeline — its outcome on one message. Idempotent: a re-run overwrites
/// with the same truth, and the Postgres mirror write follows.
///
/// # Errors
///
/// An unknown route or outcome spelling, a store failure, or a mirror-write
/// failure.
pub async fn mark_processed(
    _db: &ApplicationStore,
    message_id: Uuid,
    route: &str,
    processed_at: DateTime<Utc>,
    outcome: Option<&str>,
) -> Result<(), InboxError> {
    if !route::is_valid(route) {
        return Err(InboxError::InvalidRoute(route.to_owned()));
    }
    if let Some(spelling) = outcome {
        if !crate::inbox::nef_outcome::ALL.contains(&spelling) {
            return Err(InboxError::InvalidRoute(spelling.to_owned()));
        }
    }
    let pg = inbox_authority().await?;
    crate::postgres::inbox::mark_processed_on(&pg, message_id, route, processed_at, outcome).await
}

/// One message's process context: the row the classification layer reads
/// plus the recorded state. Visibility follows the intake gate — the owner
/// sees any firm; lawyers, clerks, and admins their firms; a missing record
/// and a firm outside the caller's membership both report not found, so
/// the door never discloses a conversation it cannot serve.
///
/// # Errors
///
/// A store failure, a denial for a client-tier actor, or not found
/// (including out-of-scope).
pub async fn process_context(
    db: &ApplicationStore,
    actor_id: Uuid,
    message_id: Uuid,
) -> Result<ProcessContext, InboxError> {
    let actor = load_person(db, actor_id).await?;
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let conversation = conversation_by_id(db, message.conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if actor.role != Role::Owner {
        if !matches!(actor.role, Role::Lawyer | Role::Clerk | Role::Admin) {
            return Err(InboxError::Denied);
        }
        let memberships = crate::firms::memberships_for_person(db, actor_id)
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        if !memberships
            .iter()
            .any(|membership| membership.firm_id == conversation.firm_id)
        {
            return Err(InboxError::NotFound);
        }
    }
    let state = process_state(db, message_id)
        .await?
        .unwrap_or(ProcessState {
            route: None,
            processed_at: None,
            nef_outcome: None,
        });
    Ok(ProcessContext {
        row: UnprocessedRow {
            id: message.id,
            conversation_id: message.conversation_id,
            firm_id: conversation.firm_id,
            project_id: conversation.project_id,
            channel: conversation.channel,
            provider_message_id: message.provider_message_id,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            body: message.body,
            occurred_at: message.occurred_at,
            inserted_at: message.inserted_at,
        },
        state,
    })
}

/// The intake gate's row plus the recorded process state, served by the
/// single-message doors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessContext {
    /// The message and conversation facts classification reads.
    pub row: UnprocessedRow,
    /// What a previous process run recorded, if anything.
    pub state: ProcessState,
}

/// Intake detail. Assigned message bodies stay hidden unless the actor
/// participates on that Project.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor lacks Firm access.
/// [`InboxError::NotFound`] when the conversation does not exist.
pub async fn intake_detail(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
) -> Result<IntakeDetail, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::intake_detail_on(
        &pg,
        db,
        actor_id,
        conversation_id,
    ))
    .await
}

/// Project inbox for a participating Lawyer, Clerk, or — read-only — any
/// other participant. Clients are refused; they read the matter
/// communication log, which omits internal notes.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor does not participate, or is a Client.
/// [`InboxError::NotFound`] when the conversation is not on that Project.
pub async fn project_thread(
    db: &ApplicationStore,
    actor_id: Uuid,
    project_id: Uuid,
    conversation_id: Uuid,
) -> Result<ThreadView, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::project_thread_on(
        &pg,
        db,
        actor_id,
        project_id,
        conversation_id,
    ))
    .await
}

/// Conversations assigned to one Project, for a participant who may open
/// the Project inbox.
///
/// # Errors
///
/// [`InboxError::Denied`] for a non-participant or a Client.
pub async fn list_project(
    db: &ApplicationStore,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<IntakeRow>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::list_project_on(
        &pg, db, actor_id, project_id,
    ))
    .await
}

/// Unread conversations assigned to one Project — the matter dashboard's
/// correspondence tile. A read-only projection over the conversations'
/// existing `unread` flag, firm-wide as the Inbox itself keeps it (there is
/// no per-participant read-state to derive a per-reader count from). No
/// authorization here: every caller resolves its own reader's access before
/// reaching this — the dashboard through the `matter_viewer` seam, the
/// project inbox through [`list_project`].
///
/// # Errors
///
/// Propagates any database error.
pub async fn unread_count_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<i64, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::unread_count_for_project_on(
        &pg, project_id,
    ))
    .await
}

/// Whether a participating Lawyer or Clerk may relay this email thread now.
///
/// The check reads the person row and the current participation, so a
/// revocation between accept and send is visible.
///
/// # Errors
///
/// Propagates any database error.
pub async fn clerk_may_relay_email_thread(
    db: &ApplicationStore,
    person_id: Uuid,
    email_conversation_id: Uuid,
) -> Result<bool, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::clerk_may_relay_email_thread_on(
        &pg,
        db,
        person_id,
        email_conversation_id,
    ))
    .await
}

/// Assignment history, oldest first.
///
/// # Errors
///
/// Propagates any database error.
pub async fn assignments(
    _db: &ApplicationStore,
    conversation_id: Uuid,
) -> Result<Vec<Assignment>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::assignments_on(&pg, conversation_id)).await
}

/// Asset id for a message the actor may read, or `None` when it has no
/// attachment. A caller who may not read the message is [`InboxError::Denied`].
///
/// # Errors
///
/// [`InboxError::Denied`] or [`InboxError::NotFound`].
pub async fn readable_attachment(
    db: &ApplicationStore,
    actor_id: Uuid,
    message_id: Uuid,
) -> Result<Option<Uuid>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::readable_attachment_on(
        &pg, db, actor_id, message_id,
    ))
    .await
}

pub async fn conversation_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Conversation>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::conversation_by_id_on(&pg, id)).await
}

pub async fn message_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Message>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::message_by_id_on(&pg, id)).await
}

// This stable key is part of the receipt's persistent idempotency contract.
pub(crate) fn acknowledgment_id(inbound_message_id: Uuid) -> Uuid {
    Uuid::from_u128(inbound_message_id.as_u128() ^ (1_u128 << 127))
}

/// The system receipt reserved for an inbound message, including an unknown send.
pub async fn acknowledgment_for(
    db: &ApplicationStore,
    inbound_message_id: Uuid,
) -> Result<Option<Message>, InboxError> {
    message_by_id(db, acknowledgment_id(inbound_message_id)).await
}

/// Reserve one automatic acknowledgment per inbound message before sending.
/// A reserved row is `unknown`: a crash must never cause a second external send.
/// This system receipt carries no Clerk authority and cannot dispatch a matter reply.
pub async fn reserve_acknowledgment(
    _db: &ApplicationStore,
    inbound: &IngestedInbound,
    body: &str,
) -> Result<Option<Uuid>, InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::reserve_acknowledgment_on(
        &pg, inbound, body,
    ))
    .await
}

/// Record provider acceptance of a reserved acknowledgment.
pub async fn acknowledgment_submitted(
    _db: &ApplicationStore,
    id: Uuid,
    provider_ref: Option<String>,
) -> Result<(), InboxError> {
    let pg = inbox_authority().await?;
    Box::pin(crate::postgres::inbox::acknowledgment_submitted_on(
        &pg,
        id,
        provider_ref,
    ))
    .await
}

pub(crate) fn advance_delivery(current: &str, incoming: &str) -> &'static str {
    if matches!(current, delivery::FAILED | delivery::SUPPRESSED) {
        return match current {
            delivery::FAILED => delivery::FAILED,
            _ => delivery::SUPPRESSED,
        };
    }
    if current == delivery::DELIVERED {
        return delivery::DELIVERED;
    }
    if current == delivery::UNKNOWN && incoming == delivery::SUBMITTED {
        return delivery::UNKNOWN;
    }
    match incoming {
        delivery::DELIVERED => delivery::DELIVERED,
        delivery::SUBMITTED => delivery::SUBMITTED,
        delivery::FAILED => delivery::FAILED,
        delivery::SUPPRESSED => delivery::SUPPRESSED,
        _ => delivery::UNKNOWN,
    }
}

pub(crate) async fn participant_still_may_reply(
    db: &ApplicationStore,
    person_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<bool, InboxError> {
    let person = match load_person(db, person_id).await {
        Ok(person) => person,
        Err(InboxError::NotFound) => return Ok(false),
        Err(error) => return Err(error),
    };
    if !matches!(person.role, Role::Lawyer | Role::Clerk) {
        return Ok(false);
    }
    let Some(project_id) = project_id else {
        return Ok(false);
    };
    sees_project(db, person_id, person.role, project_id).await
}

pub(crate) fn is_social_channel(channel_name: &str) -> bool {
    matches!(
        channel_name,
        channel::FACEBOOK
            | channel::INSTAGRAM
            | channel::THREADS
            | channel::YOUTUBE
            | channel::TIKTOK
            | channel::X
            | channel::LINKEDIN
    )
}

/// Whether a conversation's counterparty address is a phone number. A call
/// arrives from the same E.164 address a text does, so the display-only
/// contact link applies to both.
fn has_phone_counterparty(channel_name: &str) -> bool {
    matches!(channel_name, channel::SMS | channel::VOICE)
}

/// Social moderation authority belongs to the deployment Owner or the
/// conversation's Firm Admin DRI. A matter-side Lawyer/Clerk may reply only
/// while their Project participation remains active.
pub(crate) async fn social_reply_actor_still_authorized(
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation: &Conversation,
) -> Result<bool, InboxError> {
    reply_actor_authorized(db, actor_id, conversation.firm_id, conversation.project_id).await
}

/// The same authority for a Firm and optional Project, so an outbound text can
/// be authorized before its conversation exists.
pub(crate) async fn reply_actor_authorized(
    db: &ApplicationStore,
    actor_id: Uuid,
    firm_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<bool, InboxError> {
    let actor = match load_person(db, actor_id).await {
        Ok(actor) => actor,
        Err(InboxError::NotFound) => return Ok(false),
        Err(error) => return Err(error),
    };
    match actor.role {
        Role::Owner => Ok(true),
        Role::Admin => crate::firm_capability::FirmCapability::ManageBrandContent
            .resolve(db, actor.role, Some(actor_id), firm_id)
            .await
            .map(super::firm_capability::FirmCapabilityDecision::is_allowed)
            .map_err(|error| InboxError::Db(error.to_string())),
        Role::Lawyer | Role::Clerk => participant_still_may_reply(db, actor_id, project_id).await,
        Role::Client => Ok(false),
    }
}

pub(crate) async fn sees_project(
    db: &ApplicationStore,
    person_id: Uuid,
    role: Role,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    can_see_project(db, Some(person_id), role, project_id)
        .await
        .map_err(InboxError::Db)
}

pub(crate) async fn load_person(
    db: &ApplicationStore,
    person_id: Uuid,
) -> Result<persons::Person, InboxError> {
    persons::find_by_id(db, person_id)
        .await?
        .ok_or(InboxError::NotFound)
}

pub(crate) fn spine_channel(channel_name: &str, direction_value: &str) -> &'static str {
    match (channel_name, direction_value) {
        (channel::VOICE, _) => communications::channel::CALL,
        (channel::SMS, direction::OUTBOUND) => communications::channel::SMS_OUTBOUND,
        (channel::SMS, _) => communications::channel::SMS_INBOUND,
        (channel::EMAIL, direction::OUTBOUND) => communications::channel::EMAIL_OUTBOUND,
        (channel::EMAIL, _) => communications::channel::EMAIL_INBOUND,
        (channel::FACEBOOK, direction::OUTBOUND) => communications::channel::FACEBOOK_OUTBOUND,
        (channel::FACEBOOK, _) => communications::channel::FACEBOOK_INBOUND,
        (channel::INSTAGRAM, direction::OUTBOUND) => communications::channel::INSTAGRAM_OUTBOUND,
        (channel::INSTAGRAM, _) => communications::channel::INSTAGRAM_INBOUND,
        (channel::THREADS, direction::OUTBOUND) => communications::channel::THREADS_OUTBOUND,
        (channel::THREADS, _) => communications::channel::THREADS_INBOUND,
        (channel::TIKTOK, direction::OUTBOUND) => communications::channel::TIKTOK_OUTBOUND,
        (channel::TIKTOK, _) => communications::channel::TIKTOK_INBOUND,
        (channel::YOUTUBE, direction::OUTBOUND) => communications::channel::YOUTUBE_OUTBOUND,
        (channel::YOUTUBE, _) => communications::channel::YOUTUBE_INBOUND,
        (channel::X, direction::OUTBOUND) => communications::channel::X_OUTBOUND,
        (channel::X, _) => communications::channel::X_INBOUND,
        (channel::LINKEDIN, direction::OUTBOUND) => communications::channel::LINKEDIN_OUTBOUND,
        (channel::LINKEDIN, _) => communications::channel::LINKEDIN_INBOUND,
        _ => "",
    }
}

/// How much of a message body a list row previews.
pub(crate) const PREVIEW_CHAR_LIMIT: usize = 120;

/// One message body's row-preview text: whitespace collapsed to single
/// spaces, then ellipsized past [`PREVIEW_CHAR_LIMIT`]. A blank body
/// previews as nothing.
pub(crate) fn preview_snippet(body: &str) -> Option<String> {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.chars().count() <= PREVIEW_CHAR_LIMIT {
        trimmed.to_string()
    } else {
        format!(
            "{}\u{2026}",
            trimmed.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>()
        )
    })
}

/// The conversations' persisted sender associations as Person badges,
/// batched into one lookup. Conversations without a persisted association
/// badge as nothing.
pub(crate) async fn person_badges(
    db: &ApplicationStore,
    conversations: &[Conversation],
) -> Result<HashMap<Uuid, PersonBadge>, InboxError> {
    let ids: Vec<Uuid> = conversations
        .iter()
        .filter_map(|conversation| conversation.person_id)
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let people = persons::find_by_ids(db, &ids)
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(people
        .into_iter()
        .map(|person| {
            (
                person.id,
                PersonBadge {
                    person_id: person.id,
                    name: person.name,
                },
            )
        })
        .collect())
}

pub(crate) async fn assignee_label(
    db: &ApplicationStore,
    project_id: Option<Uuid>,
) -> Result<String, InboxError> {
    let Some(project_id) = project_id else {
        return Ok("Unassigned".to_string());
    };
    Ok(projects::find_by_id(db, project_id)
        .await?
        .map_or_else(|| "Unassigned".to_string(), |project| project.name))
}

/// The Firm's Projects a conversation may be assigned to: only open ones —
/// a closed or archived matter is neither offered nor accepted. The reading
/// pane's picker, the triage override form, and the classifier's menu all
/// read this list.
pub async fn projects_in_firm(
    _db: &ApplicationStore,
    firm_id: Uuid,
) -> Result<Vec<ProjectChoice>, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::projects_in_firm_on(&pg, firm_id).await
}

/// Shared transport handle for callers that dispatch after a request returns.
pub type SharedTransport = Arc<dyn OutboundTransport>;

/// Messages for an already-authorized reporting Conversation, from its active
/// authority. The caller must resolve visibility before supplying this ID.
pub(crate) async fn messages_for_reporting(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Vec<Message>, InboxError> {
    let pg = inbox_authority().await?;
    crate::postgres::inbox::messages_for_on(&pg, id).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::firms::{self, FirmMembership, NewFirm, NewPersonFirmRole};
    use crate::persons::{self, NewPerson};
    use crate::projects::{self, NewProject};
    use crate::test_support::seed_entity;

    struct RecordingTransport {
        calls: Mutex<Vec<OutboundEnvelope>>,
        outcome: Mutex<TransportOutcome>,
    }

    impl RecordingTransport {
        fn new(outcome: TransportOutcome) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome: Mutex::new(outcome),
            }
        }

        fn calls(&self) -> Vec<OutboundEnvelope> {
            self.calls.lock().expect("transport calls").clone()
        }
    }

    #[async_trait]
    impl OutboundTransport for RecordingTransport {
        async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
            self.calls
                .lock()
                .expect("transport calls")
                .push(envelope.clone());
            Ok(self.outcome.lock().expect("transport outcome").clone())
        }
    }

    pub(super) struct World {
        pub(super) firm: Uuid,
        other_firm: Uuid,
        project: Uuid,
        other_project: Uuid,
        foreign_project: Uuid,
        owner: Uuid,
        admin: Uuid,
        other_admin: Uuid,
        lawyer: Uuid,
        clerk: Uuid,
        client: Uuid,
        outsider: Uuid,
        clerk_participation: Uuid,
    }

    pub(super) async fn world(db: &ApplicationStore) -> World {
        let owner = person(db, "Owner", "owner@example.com", Role::Owner).await;
        let admin = person(db, "Admin", "admin@example.com", Role::Admin).await;
        let other_admin = person(db, "Other Admin", "other-admin@example.com", Role::Admin).await;
        let lawyer = person(db, "Lawyer", "lawyer@example.com", Role::Lawyer).await;
        let clerk = person(db, "Clerk", "clerk@example.com", Role::Clerk).await;
        let client = person(db, "Client", "client@example.com", Role::Client).await;
        let outsider = person(db, "Outsider", "outsider@example.com", Role::Lawyer).await;
        let firm = open_firm(db, "Practice", admin).await;
        let other_firm = open_firm(db, "Other Practice", other_admin).await;
        for (person_id, firm_id, membership, is_dri) in [
            (lawyer, firm, FirmMembership::Lawyer, false),
            (clerk, firm, FirmMembership::Clerk, false),
            (other_admin, other_firm, FirmMembership::Admin, true),
        ] {
            firms::ensure_membership(
                db,
                &NewPersonFirmRole {
                    person_id,
                    firm_id,
                    membership,
                    is_dri,
                },
            )
            .await
            .unwrap();
        }
        let project = open_project(db, "alpha-matter", "Alpha Matter", firm).await;
        let other_project = open_project(db, "beta-matter", "Beta Matter", firm).await;
        let foreign_project =
            open_project(db, "foreign-matter", "Foreign Matter", other_firm).await;
        projects::designate_dri(db, project, lawyer, projects::DriSide::Lawyer)
            .await
            .unwrap();
        projects::designate_dri(db, other_project, lawyer, projects::DriSide::Lawyer)
            .await
            .unwrap();
        let clerk_participation = projects::add_participation(
            db,
            project,
            clerk,
            projects::participation_for_role(Role::Clerk),
        )
        .await
        .unwrap();
        projects::add_participation(
            db,
            other_project,
            clerk,
            projects::participation_for_role(Role::Clerk),
        )
        .await
        .unwrap();
        projects::add_participation(
            db,
            project,
            client,
            projects::participation_for_role(Role::Client),
        )
        .await
        .unwrap();
        World {
            firm,
            other_firm,
            project,
            other_project,
            foreign_project,
            owner,
            admin,
            other_admin,
            lawyer,
            clerk,
            client,
            outsider,
            clerk_participation: clerk_participation.id,
        }
    }

    async fn person(db: &ApplicationStore, name: &str, email: &str, role: Role) -> Uuid {
        persons::create(db, &NewPerson::with_role(name, email, role))
            .await
            .unwrap()
            .id
    }

    async fn open_firm(db: &ApplicationStore, name: &str, admin_id: Uuid) -> Uuid {
        firms::create(
            db,
            &NewFirm {
                name: name.into(),
                status: "active".into(),
                entity_id: seed_entity(db).await,
                admin_dri_person_id: admin_id,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn open_project(db: &ApplicationStore, code: &str, name: &str, firm_id: Uuid) -> Uuid {
        projects::create(
            db,
            &NewProject {
                code: code.into(),
                name: name.into(),
                status: "open".into(),
                entity_id: seed_entity(db).await,
                firm_id: Some(firm_id),
                ..NewProject::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn inbound<'a>(
        firm: Uuid,
        channel_name: &'a str,
        account: &'a str,
        provider_message_id: &'a str,
        address: &'a str,
    ) -> IngestInbound<'a> {
        IngestInbound {
            project_id: None,
            firm_id: firm,
            brand_key: None,
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: channel_name,
            provider_account: account,
            provider_message_id,
            provider_thread_ref: None,
            external_address: address,
            external_name: Some("Sender"),
            subject: Some("Question"),
            body: "Hello from outside.",
            source_page: Some("/contact"),
            source_channel: Some(channel_name),
            matter_consent: true,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            occurred_at: "2026-09-27T00:00:00Z",
            raw_storage_key: None,
        }
    }

    fn attachment(asset_id: Uuid, filename: &str, sha: &str) -> IngestAttachment {
        IngestAttachment {
            asset_id,
            filename: filename.to_string(),
            sha256: sha.to_string(),
        }
    }

    macro_rules! with_native_inbox_fixture {
        ($db:ident, $pg:ident, $body:block) => {
            crate::test_support::with_native_application_fixture(|$db, fixture_pg| async move {
                #[allow(unused_variables)]
                let $pg = fixture_pg;
                $body
            })
            .await
        };
    }

    #[tokio::test]
    async fn attachments_ingest_as_ordered_assets() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        for (asset_id, sha256) in [(first, "sha-complaint"), (second, "sha-contract")] {
            sqlx::query("INSERT INTO asset (id, project_id, storage_key, content_type, byte_size, sha256_hex) VALUES ($1, $2, 'synthetic/inbox-test', 'application/pdf', 1, $3)")
                .bind(asset_id)
                .bind(world.project)
                .bind(sha256)
                .execute(pg.pool())
                .await
                .unwrap();
        }
        let mut message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attachments-1",
            "client@example.com",
        );
        message.attachments = vec![
            attachment(first, "complaint.pdf", "sha-complaint"),
            attachment(second, "contract.docx", "sha-contract"),
        ];
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        assert!(!ingested.deduped);

        let linked = message_assets(&db, world.firm, ingested.message_id)
            .await
            .unwrap();
        assert_eq!(linked.len(), 2);
        assert_eq!(linked[0].asset_id, first);
        assert_eq!(linked[0].filename, "complaint.pdf");
        assert_eq!(linked[0].position, 0);
        assert_eq!(linked[0].sha256, "sha-complaint");
        assert_eq!(linked[1].asset_id, second);
        assert_eq!(linked[1].position, 1);

        let stored = message_by_id(&db, ingested.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.attachment_asset_id, Some(first));

        });
    }

    #[tokio::test]
    async fn an_alias_sender_resolves_to_the_mailbox_holder() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        persons::add_alias(&db, world.client, "client-alias@example.com")
            .await
            .unwrap();
        let message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "alias-sender-1",
            "client-alias@example.com",
        );
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        let conversation = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        // The alias sender IS the client: the person_mailbox claim resolves
        // where the primary-email lookup missed, and the link persists.
        assert_eq!(conversation.person_id, Some(world.client));
        let contact = sender_identity(&db, &conversation).await.unwrap().unwrap();
        assert_eq!(contact.person_id, world.client);

        });
    }

    /// Counts Person rows holding an exact email — the auto-create dedup
    /// probe for the tests below.
    async fn person_count(pg: &crate::postgres::PostgresDb, email: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM person WHERE lower(email) = lower($1)")
            .bind(email)
            .fetch_one(pg.pool())
            .await
            .expect("person count query")
    }

    #[tokio::test]
    async fn an_unknown_sender_persists_a_created_person_and_one_relationship_log() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "unknown-sender-1",
            "new-counterparty@example.com",
        );
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        let conversation = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        // Exactly one Person appears: named from the RFC From display name,
        // the sender's address as primary email, client-tier by default.
        let created = persons::find_by_any_email(&db, "new-counterparty@example.com")
            .await
            .unwrap()
            .expect("created person");
        assert_eq!(conversation.person_id, Some(created.id));
        assert_eq!(person_count(&pg, "new-counterparty@example.com").await, 1);
        assert_eq!(created.name, "Sender");
        assert_eq!(created.role, Role::Client);
        // One Relationship Log entry — the lawyer-acknowledged feed — and
        // never a typed relationship edge.
        let logs = crate::relationship_logs::all(&db).await.unwrap();
        let associations: Vec<_> = logs
            .iter()
            .filter(|log| log.action == INBOUND_ASSOCIATION_ACTION)
            .collect();
        assert_eq!(associations.len(), 1);
        assert_eq!(associations[0].subject_type, "person");
        assert_eq!(associations[0].subject_id, created.id);

        });
    }

    #[tokio::test]
    async fn failed_association_log_rolls_back_inbound_ingest() {
        with_native_inbox_fixture!(db, pg, {
            let world = world(&db).await;
            persons::create(
                &db,
                &NewPerson::new("Sender", "rollback-sender@example.com"),
            )
            .await
            .unwrap();
            sqlx::query(
                "ALTER TABLE relationship_log ADD CONSTRAINT reject_inbox_log \
                 CHECK (action <> 'inbound_email_associated')",
            )
            .execute(pg.pool())
            .await
            .unwrap();
            let message = inbound(
                world.firm,
                channel::EMAIL,
                "fixture-account",
                "rollback-log",
                "rollback-sender@example.com",
            );
            assert!(ingest_inbound(&db, &message).await.is_err());
            let conversations: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM inbox_conversation WHERE external_address = $1",
            )
            .bind("rollback-sender@example.com")
            .fetch_one(pg.pool())
            .await
            .unwrap();
            let messages: i64 =
                sqlx::query_scalar("SELECT count(*) FROM message WHERE provider_message_id = $1")
                    .bind("rollback-log")
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
            assert_eq!(conversations, 0);
            assert_eq!(messages, 0);
        });
    }

    #[tokio::test]
    async fn concurrent_replays_commit_one_sender_association_and_log() {
        with_native_inbox_fixture!(db, pg, {
            let world = world(&db).await;
            let sender = persons::create(
                &db,
                &NewPerson::new("Concurrent Sender", "concurrent-sender@example.com"),
            )
            .await
            .unwrap();
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
            let mut tasks = Vec::new();
            for _ in 0..8 {
                let pg = pg.clone();
                let barrier = barrier.clone();
                let firm = world.firm;
                tasks.push(tokio::spawn(crate::postgres::PostgresDb::with_test_db(
                    pg,
                    async move {
                        let db = ApplicationStore::uninitialized();
                        let message = inbound(
                            firm,
                            channel::EMAIL,
                            "fixture-account",
                            "concurrent-replay",
                            "concurrent-sender@example.com",
                        );
                        barrier.wait().await;
                        ingest_inbound(&db, &message).await.unwrap()
                    },
                )));
            }
            let mut deliveries = Vec::new();
            for task in tasks {
                deliveries.push(task.await.unwrap());
            }
            let first = &deliveries[0];
            assert!(deliveries
                .iter()
                .all(|item| item.message_id == first.message_id));
            assert!(deliveries
                .iter()
                .all(|item| item.conversation_id == first.conversation_id));
            assert_eq!(deliveries.iter().filter(|item| !item.deduped).count(), 1);
            let conversation = conversation_by_id(&db, first.conversation_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(conversation.person_id, Some(sender.id));
            let logs = crate::relationship_logs::all(&db).await.unwrap();
            assert_eq!(
                logs.iter()
                    .filter(|log| log.action == INBOUND_ASSOCIATION_ACTION
                        && log.subject_id == sender.id)
                    .count(),
                1
            );
        });
    }

    #[tokio::test]
    async fn a_reingest_from_the_same_address_creates_no_second_person_or_log() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        for message_id in ["unknown-sender-2", "unknown-sender-3"] {
            let message = inbound(
                world.firm,
                channel::EMAIL,
                "fixture-account",
                message_id,
                "repeat-counterparty@example.com",
            );
            let ingested = ingest_inbound(&db, &message).await.unwrap();
            let conversation = conversation_by_id(&db, ingested.conversation_id)
                .await
                .unwrap()
                .unwrap();
            assert!(conversation.person_id.is_some());
        }
        // The resolver dedups: one Person and one Relationship Log entry no
        // matter how many deliveries arrive from the same address.
        assert_eq!(
            person_count(&pg, "repeat-counterparty@example.com").await,
            1
        );
        let logs = crate::relationship_logs::all(&db).await.unwrap();
        assert_eq!(
            logs.iter()
                .filter(|log| log.action == INBOUND_ASSOCIATION_ACTION)
                .count(),
            1
        );

        });
    }

    /// Scraped notices enter the same ingest door as email and SMS: the
    /// `scraper` channel is supported, the message dedupes on the run-spanning
    /// notice identity, and triage sees it like any other intake.
    #[tokio::test]
    async fn scraper_channel_ingests_and_dedupes_like_other_intake() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
        let world = world(&db).await;
        let asset_id = Uuid::now_v7();
        sqlx::query("INSERT INTO asset (id, project_id, storage_key, content_type, byte_size, sha256_hex) VALUES ($1, $2, 'synthetic/notice.pdf', 'application/pdf', 3, 'sha-dkt')")
            .bind(asset_id)
            .bind(world.project)
            .execute(pg.pool())
            .await
            .unwrap();
        let mut message = inbound(
            world.firm,
            channel::SCRAPER,
            "la-superior",
            "la-superior/0004/4",
            "0004",
        );
        message.attachments = vec![attachment(
            asset_id,
            "DKT 0004 - Notice of Hearing.pdf",
            "sha-dkt",
        )];
        let first = ingest_inbound(&db, &message).await.unwrap();
        assert!(!first.deduped);

        // A retry after a lost acknowledgment returns the original row.
        let second = ingest_inbound(&db, &message).await.unwrap();
        assert!(second.deduped);
        assert_eq!(second.message_id, first.message_id);
        });
    }

    #[tokio::test]
    async fn ingest_time_project_attribution_scopes_thread_matching() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut attributed = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attributed-1",
            "client@example.com",
        );
        attributed.project_id = Some(world.project);
        let first = ingest_inbound(&db, &attributed).await.unwrap();
        let stored = conversation_by_id(&db, first.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.project_id, Some(world.project));

        // A second attributed delivery joins the attributed thread.
        let mut follow_up = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attributed-2",
            "client@example.com",
        );
        follow_up.project_id = Some(world.project);
        let second = ingest_inbound(&db, &follow_up).await.unwrap();
        assert_eq!(second.conversation_id, first.conversation_id);
        assert!(!second.deduped);

        // An unattributed delivery from the same address never silently
        // adopts the attributed thread; it starts its own.
        let unattributed = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attributed-3",
            "client@example.com",
        );
        let third = ingest_inbound(&db, &unattributed).await.unwrap();
        assert_ne!(third.conversation_id, first.conversation_id);
        let own_thread = conversation_by_id(&db, third.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(own_thread.project_id, None);

        });
    }

    async fn assigned_sms(db: &ApplicationStore, world: &World) -> Uuid {
        let ingested = ingest_inbound(
            db,
            &inbound(
                world.firm,
                channel::SMS,
                "fixture-account",
                "sms-1",
                "sms:fixture-sender",
            ),
        )
        .await
        .unwrap();
        assign(db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        ingested.conversation_id
    }

    #[tokio::test]
    async fn owner_assigns_inside_the_firm_and_projects_communications() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let stored = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.source_page.as_deref(), Some("/contact"));
        assert_eq!(stored.source_channel.as_deref(), Some(channel::EMAIL));
        assert!(communications::for_project(&db, world.project)
            .await
            .unwrap()
            .is_empty());

        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let spine = communications::for_project(&db, world.project)
            .await
            .unwrap();
        assert_eq!(spine.len(), 1);
        assert_eq!(spine[0].body, "Hello from outside.");
        assert_eq!(spine[0].project_id, world.project);
        let audit = assignments(&db, ingested.conversation_id).await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].to_project_id, world.project);
        assert!(audit[0].from_project_id.is_none());

        });
    }

    async fn trashed(db: &ApplicationStore, actor: Uuid, conversation: Uuid) -> bool {
        list_intake(db, actor)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == conversation)
            .unwrap()
            .trashed
    }

    #[tokio::test]
    async fn trash_hides_a_conversation_until_restore_or_new_mail() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
        let world = world(&db).await;
        let first = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-1",
            "sender@example.com",
        );
        let id = ingest_inbound(&db, &first).await.unwrap().conversation_id;
        assign(&db, world.owner, id, world.project).await.unwrap();
        assert_eq!(
            unread_count_for_project(&db, world.project).await.unwrap(),
            1
        );

        for actor in [world.admin, world.client, world.outsider] {
            assert!(matches!(
                set_trashed(&db, actor, id, true).await,
                Err(InboxError::Denied)
            ));
        }
        assert!(!trashed(&db, world.owner, id).await);

        set_trashed(&db, world.lawyer, id, true).await.unwrap();
        assert!(trashed(&db, world.owner, id).await);
        assert_eq!(
            unread_count_for_project(&db, world.project).await.unwrap(),
            0
        );
        // Nothing is removed: the thread and its messages still read.
        assert_eq!(crate::postgres::inbox::messages_for_on(&pg, id).await.unwrap().len(), 1);

        set_trashed(&db, world.clerk, id, false).await.unwrap();
        assert!(!trashed(&db, world.owner, id).await);

        set_trashed(&db, world.owner, id, true).await.unwrap();
        let mut second = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-2",
            "sender@example.com",
        );
        second.project_id = Some(world.project);
        assert_eq!(
            ingest_inbound(&db, &second).await.unwrap().conversation_id,
            id
        );
        assert!(!trashed(&db, world.owner, id).await);
        });
    }

    #[tokio::test]
    async fn the_project_menu_offers_only_open_projects_and_the_door_denies_the_rest() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "closed-target",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        for status in ["closed", "archived", "Open "] {
            sqlx::query("UPDATE project SET status = $1 WHERE id = $2")
                .bind(status.trim())
                .bind(world.project)
                .execute(pg.pool())
                .await
                .unwrap();
            // The menu never offers a matter that is not open, and the door
            // refuses a hand-crafted POST into one — the same rule twice.
            let menu = projects_in_firm(&db, world.firm).await.unwrap();
            assert_eq!(
                menu.iter().any(|project| project.id == world.project),
                status == "Open ",
                "{status}: {menu:?}"
            );
            let outcome =
                attribute_to_project(&db, world.owner, ingested.conversation_id, world.project)
                    .await;
            if status == "Open " {
                outcome.unwrap();
            } else {
                assert!(matches!(outcome, Err(InboxError::Denied)), "{status}");
            }
        }

        });
    }

    #[tokio::test]
    async fn ordinary_assignment_authorization_uses_source_roles_and_supervised_participation() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        for actor in [world.owner, world.lawyer, world.clerk] {
            require_assignment_authorization(&db, actor, world.firm, world.project)
                .await
                .unwrap();
        }
        for actor in [world.admin, world.client, world.outsider] {
            assert!(matches!(
                require_assignment_authorization(&db, actor, world.firm, world.project).await,
                Err(InboxError::Denied)
            ));
        }
        assert!(matches!(
            require_assignment_authorization(&db, world.owner, world.firm, world.foreign_project)
                .await,
            Err(InboxError::CrossFirm)
        ));
        assert!(firms::memberships_for_person(&db, world.outsider)
            .await
            .unwrap()
            .is_empty());
        projects::add_participation(
            &db,
            world.project,
            world.outsider,
            projects::participation_for_role(Role::Lawyer),
        )
        .await
        .unwrap();
        require_assignment_authorization(&db, world.outsider, world.firm, world.project)
            .await
            .unwrap();
        projects::remove_participation(&db, world.clerk_participation)
            .await
            .unwrap();
        assert!(matches!(
            require_assignment_authorization(&db, world.clerk, world.firm, world.project).await,
            Err(InboxError::Denied)
        ));

        });
    }

    #[tokio::test]
    async fn cross_firm_assignment_is_denied_and_non_owners_cannot_assign() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let error = assign(
            &db,
            world.owner,
            ingested.conversation_id,
            world.foreign_project,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InboxError::CrossFirm));
        for actor in [world.admin, world.client] {
            let error = assign(&db, actor, ingested.conversation_id, world.project)
                .await
                .unwrap_err();
            assert!(matches!(error, InboxError::Denied), "{actor}");
        }
        for actor in [world.lawyer, world.clerk] {
            assign(&db, actor, ingested.conversation_id, world.project)
                .await
                .unwrap();
        }
        assert!(list_intake(&db, world.admin).await.is_ok());
        assert!(list_intake(&db, world.lawyer).await.is_ok());

        });
    }

    #[tokio::test]
    async fn outsiders_and_clients_cannot_read_an_assigned_thread() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        assert!(
            project_thread(&db, world.outsider, world.project, conversation_id)
                .await
                .is_err()
        );
        assert!(
            project_thread(&db, world.client, world.project, conversation_id)
                .await
                .is_err()
        );

        });
    }

    #[tokio::test]
    async fn a_participant_lists_the_project_inbox_and_a_client_cannot() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        for actor in [world.client, world.outsider] {
            let error = list_project(&db, actor, world.project).await.unwrap_err();
            assert!(matches!(error, InboxError::Denied), "{actor}");
        }
        let rows = list_project(&db, world.clerk, world.project).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, conversation_id);
        assert_eq!(rows[0].channel, channel::SMS);
        assert_eq!(rows[0].next_action, "Send a reply");
        let lawyer_rows = list_project(&db, world.lawyer, world.project)
            .await
            .unwrap();
        assert_eq!(lawyer_rows.len(), 1);
        assert_eq!(lawyer_rows[0].next_action, "Send a reply");

        });
    }

    #[test]
    fn next_action_names_each_viewers_move() {
        assert_eq!(
            next_action(Role::Owner, false, delivery::RECEIVED, false),
            "Assign to a project"
        );
        assert_eq!(
            next_action(Role::Owner, true, delivery::RECEIVED, false),
            "Reassign"
        );
        assert_eq!(
            next_action(Role::Clerk, true, delivery::UNKNOWN, true),
            "Reconcile delivery"
        );
        assert_eq!(
            next_action(Role::Clerk, true, delivery::SUBMITTED, true),
            "Send a reply"
        );
        assert_eq!(
            next_action(Role::Lawyer, true, delivery::RECEIVED, false),
            "Leave an internal note"
        );
        assert_eq!(
            next_action(Role::Admin, true, delivery::RECEIVED, false),
            "Review"
        );
    }

    #[test]
    fn unknown_channels_do_not_project_as_email() {
        assert_eq!(spine_channel("mastodon", direction::INBOUND), "");
        assert_eq!(spine_channel("unsupported", direction::OUTBOUND), "");
        assert_eq!(
            spine_channel(channel::INSTAGRAM, direction::INBOUND),
            communications::channel::INSTAGRAM_INBOUND
        );
        assert_eq!(
            spine_channel(channel::THREADS, direction::OUTBOUND),
            communications::channel::THREADS_OUTBOUND
        );
        assert_eq!(
            spine_channel(channel::EMAIL, direction::INBOUND),
            communications::channel::EMAIL_INBOUND
        );
        assert_eq!(
            spine_channel(channel::SMS, direction::OUTBOUND),
            communications::channel::SMS_OUTBOUND
        );
    }

    #[tokio::test]
    async fn a_participating_lawyer_can_reply_and_elevated_roles_cannot_send() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        let lawyer = project_thread(&db, world.lawyer, world.project, conversation_id)
            .await
            .unwrap();
        assert!(lawyer.can_note);
        assert!(lawyer.can_send);
        assert_eq!(lawyer.next_action, "Send a reply");
        post_note(&db, world.lawyer, conversation_id, "Private strategy.")
            .await
            .unwrap();
        let reply = queue_reply(
            &db,
            world.lawyer,
            conversation_id,
            "A participating lawyer can reply.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .author_person_id,
            Some(world.lawyer)
        );
        for (actor, body) in [
            (world.owner, "An owner cannot send this."),
            (world.admin, "An admin cannot send this."),
            (world.outsider, "An outsider cannot send this."),
        ] {
            assert!(
                queue_reply(&db, actor, conversation_id, body, purpose::MATTER)
                    .await
                    .is_err(),
                "{actor}"
            );
        }

        });
    }

    #[tokio::test]
    async fn a_lawyer_who_does_not_participate_cannot_reply() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        assert!(queue_reply(
            &db,
            world.outsider,
            conversation_id,
            "Not authorized.",
            purpose::MATTER,
        )
        .await
        .is_err());

        });
    }

    #[tokio::test]
    async fn a_clerk_send_omits_the_internal_note() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        post_note(&db, world.lawyer, conversation_id, "Private strategy.")
            .await
            .unwrap();
        let clerk = project_thread(&db, world.clerk, world.project, conversation_id)
            .await
            .unwrap();
        assert!(clerk.can_note);
        assert!(clerk.can_send);
        assert_eq!(clerk.next_action, "Send a reply");
        let reply = queue_reply(
            &db,
            world.clerk,
            conversation_id,
            "We received your message.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "provider-1".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.submitted, 1);
        let calls = transport.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].channel, channel::SMS);
        assert_eq!(calls[0].body, "We received your message.");
        assert!(!calls[0].body.contains("Private strategy"));
        let sent = message_by_id(&db, reply).await.unwrap().unwrap();
        assert_eq!(sent.delivery_state, delivery::SUBMITTED);
        let firm_view = communications::for_project(&db, world.project)
            .await
            .unwrap();
        let client_view = communications::for_project_client_visible(&db, world.project)
            .await
            .unwrap();
        assert!(firm_view.iter().any(|row| row.body == "Private strategy."));
        assert!(client_view
            .iter()
            .all(|row| row.body != "Private strategy."));
        assert!(client_view
            .iter()
            .any(|row| row.body == "We received your message."));

        });
    }

    #[tokio::test]
    async fn reassignment_moves_history_and_revokes_the_old_project() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        assign(
            &db,
            world.owner,
            ingested.conversation_id,
            world.other_project,
        )
        .await
        .unwrap();
        assert!(communications::for_project(&db, world.project)
            .await
            .unwrap()
            .is_empty());
        let moved = communications::for_project(&db, world.other_project)
            .await
            .unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].body, "Hello from outside.");
        assert!(
            project_thread(&db, world.lawyer, world.project, ingested.conversation_id)
                .await
                .is_err()
        );
        let audit = assignments(&db, ingested.conversation_id).await.unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[1].from_project_id, Some(world.project));
        assert_eq!(audit[1].to_project_id, world.other_project);

        });
    }

    #[tokio::test]
    async fn revoked_participation_suppresses_a_queued_send() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let reply = queue_reply(
            &db,
            world.clerk,
            ingested.conversation_id,
            "Still drafting.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        projects::remove_participation(&db, world.clerk_participation)
            .await
            .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "should-not-send".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.suppressed, 1);
        assert!(transport.calls().is_empty());
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUPPRESSED
        );

        });
    }

    #[tokio::test]
    async fn revoked_lawyer_participation_suppresses_a_queued_send() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "lawyer-msg",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let reply = queue_reply(
            &db,
            world.lawyer,
            ingested.conversation_id,
            "Drafted by counsel.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let lawyer_participation = projects::participations_for_project(&db, world.project)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.person_id == world.lawyer)
            .unwrap();
        projects::remove_participation(&db, lawyer_participation.id)
            .await
            .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "should-not-send".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.suppressed, 1);
        assert!(transport.calls().is_empty());
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUPPRESSED
        );

        });
    }

    #[tokio::test]
    async fn duplicate_webhooks_do_not_create_a_second_message() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let first = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let second = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(second.deduped);
        assert_eq!(first.message_id, second.message_id);
        let other_account = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "other-account",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(!other_account.deduped);
        assert_ne!(other_account.message_id, first.message_id);
        let other_firm = ingest_inbound(
            &db,
            &inbound(
                world.other_firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(!other_firm.deduped);

        });
    }

    #[tokio::test]
    async fn tiktok_public_comment_reply_projects_explicit_channels_and_refuses_private_audience() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut input = inbound(
            world.firm,
            channel::TIKTOK,
            "synthetic-account",
            "comment-1",
            "reader",
        );
        input.interaction_type = Some("comment");
        input.visibility = Some("public");
        input.provider_post_id = Some("video-1");
        let received = ingest_inbound(&db, &input).await.unwrap();
        assign(&db, world.lawyer, received.conversation_id, world.project)
            .await
            .unwrap();
        assert!(queue_social_reply(
            &db,
            world.lawyer,
            received.conversation_id,
            "Private response",
            "private"
        )
        .await
        .is_err());
        queue_social_reply(
            &db,
            world.lawyer,
            received.conversation_id,
            "Public response",
            "public",
        )
        .await
        .unwrap();
        for unauthorized in [world.other_admin, world.client, world.outsider] {
            assert!(queue_social_reply(
                &db,
                unauthorized,
                received.conversation_id,
                "Cross-Firm response",
                "public"
            )
            .await
            .is_err());
        }
        input.provider_post_id = None;
        input.provider_message_id = "no-video-comment";
        input.external_address = "another-reader";
        let no_video = ingest_inbound(&db, &input).await.unwrap();
        assert!(queue_social_reply(
            &db,
            world.owner,
            no_video.conversation_id,
            "Missing context",
            "public"
        )
        .await
        .is_err());
        let rows = communications::for_project(&db, world.project)
            .await
            .unwrap();
        assert!(rows
            .iter()
            .any(|row| row.channel == communications::channel::TIKTOK_INBOUND));
        assert!(rows
            .iter()
            .any(|row| row.channel == communications::channel::TIKTOK_OUTBOUND));

        });
    }

    async fn assert_instagram_replies_are_attributed(
        db: &ApplicationStore,
        actor: Uuid,
        project_id: Uuid,
        conversation_id: Uuid,
    ) {
        assign(db, actor, conversation_id, project_id)
            .await
            .unwrap();
        for (body, visibility) in [
            ("Public response", "public"),
            ("Private response", "private"),
        ] {
            let reply = queue_social_reply(db, actor, conversation_id, body, visibility)
                .await
                .unwrap();
            assert_eq!(
                message_by_id(db, reply)
                    .await
                    .unwrap()
                    .unwrap()
                    .visibility
                    .as_deref(),
                Some(visibility)
            );
        }
    }

    async fn assert_threads_refuses_private_reply(
        db: &ApplicationStore,
        world: &World,
        inbound: &IngestInbound<'_>,
    ) {
        let mut threads_inbound = inbound.clone();
        threads_inbound.channel = channel::THREADS;
        threads_inbound.provider_account = "threads-account";
        threads_inbound.provider_message_id = "thread-reply";
        let received = ingest_inbound(db, &threads_inbound).await.unwrap();
        assign(db, world.lawyer, received.conversation_id, world.project)
            .await
            .unwrap();
        assert!(queue_social_reply(
            db,
            world.lawyer,
            received.conversation_id,
            "Private response",
            "private",
        )
        .await
        .is_err());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn social_comment_keeps_brand_post_thread_and_projects_explicit_channel() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut inbound = inbound(
            world.firm,
            channel::INSTAGRAM,
            "ig-account",
            "comment-1",
            "sender",
        );
        inbound.brand_key = Some("neon");
        inbound.interaction_type = Some("comment");
        inbound.provider_post_id = Some("ig-post-42");
        inbound.provider_thread_ref = Some("ig-thread-9");
        inbound.visibility = Some("public");
        let received = ingest_inbound(&db, &inbound).await.unwrap();
        let conversation = conversation_by_id(&db, received.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.brand_key.as_deref(), Some("neon"));
        assert_eq!(conversation.provider_post_id.as_deref(), Some("ig-post-42"));
        assert_eq!(
            conversation.provider_thread_ref.as_deref(),
            Some("ig-thread-9")
        );
        assert_eq!(conversation.interaction_type, "comment");
        assert_eq!(
            message_by_id(&db, received.message_id)
                .await
                .unwrap()
                .unwrap()
                .visibility
                .as_deref(),
            Some("public")
        );
        assert_instagram_replies_are_attributed(
            &db,
            world.lawyer,
            world.project,
            received.conversation_id,
        )
        .await;
        assert_threads_refuses_private_reply(&db, &world, &inbound).await;
        let messages = communications::for_project(&db, world.project)
            .await
            .unwrap();
        assert!(messages.iter().any(|message| {
            message.channel == communications::channel::INSTAGRAM_INBOUND
                && message.source_ref.as_deref()
                    == Some(format!("inbox:{}", received.message_id).as_str())
        }));

        });
    }

    #[tokio::test]
    async fn owner_opens_a_text_with_consent_and_replies_in_the_same_thread() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let open = |actor: Uuid, to: &'static str, consent: bool| {
            let db = db.clone();
            let firm = world.firm;
            async move {
                open_sms_conversation(
                    &db,
                    actor,
                    firm,
                    Some("neon"),
                    "+15550100000",
                    to,
                    "Hello from the firm",
                    consent,
                )
                .await
            }
        };

        assert!(matches!(
            open(world.owner, "+15555550123", false).await,
            Err(InboxError::Denied)
        ));
        for bad in ["5555550123", "+0555550123", "+1555abc0123", "+15550100000"] {
            assert!(matches!(
                open(world.owner, bad, true).await,
                Err(InboxError::Denied)
            ));
        }
        assert!(matches!(
            open(world.other_admin, "+15555550123", true).await,
            Err(InboxError::Denied)
        ));
        assert!(matches!(
            open(world.outsider, "+15555550123", true).await,
            Err(InboxError::Denied)
        ));
        assert!(matches!(
            open_sms_conversation(
                &db,
                world.owner,
                world.firm,
                None,
                "+15550100000",
                "+15555550123",
                "   ",
                true,
            )
            .await,
            Err(InboxError::Empty)
        ));

        let (conversation_id, first_message) = open(world.owner, "+15555550123", true).await.unwrap();
        let (again, _) = open(world.owner, "+15555550123", true).await.unwrap();
        assert_eq!(conversation_id, again);
        let conversation = conversation_by_id(&db, conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.channel, channel::SMS);
        assert_eq!(conversation.external_address, "+15555550123");
        assert!(conversation.matter_consent);

        queue_sms_reply(&db, world.owner, conversation_id, "Second text")
            .await
            .unwrap();
        assert!(
            queue_sms_reply(&db, world.other_admin, conversation_id, "Cross-Firm")
                .await
                .is_err()
        );
        let detail = intake_detail(&db, world.owner, conversation_id)
            .await
            .unwrap();
        assert!(detail.can_sms_reply);
        assert!(detail.bodies_visible);

        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "SM1".into(),
        });
        let report = dispatch_queued_message(&db, &transport, first_message)
            .await
            .unwrap();
        assert_eq!(report.submitted, 1);
        assert_eq!(
            message_by_id(&db, first_message)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUBMITTED
        );
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.submitted, 2);
        assert_eq!(report.suppressed, 0);
        let calls = transport.calls();
        assert!(calls.iter().all(|call| {
            call.channel == channel::SMS
                && call.account == "+15550100000"
                && call.to == "+15555550123"
        }));

        });
    }

    #[tokio::test]
    async fn intake_pages_keep_firm_scope_filters_and_email_date_order() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
            let world = world(&db).await;
            let mut tied = Vec::new();
            let mut oldest = None;
            let mut newest = None;
            for (i, firm, brand, occurred_at) in [
                (0, world.firm, "neon", "2026-10-14T08:00:00.000000001-04:00"),
                (1, world.firm, "neon", "2026-10-14T12:00:00.000000001Z"),
                (2, world.firm, "neon", "2026-10-13T23:00:00Z"),
                (3, world.other_firm, "neon", "2026-10-15T00:00:00Z"),
                (4, world.firm, "vesta", "2026-10-16T00:00:00Z"),
                (5, world.firm, "neon", "2026-10-14T12:00:00.000000002Z"),
            ] {
                let provider_id = format!("paged-{i}");
                let sender = format!("paged-{i}@example.test");
                let mut message = inbound(firm, channel::EMAIL, "paged", &provider_id, &sender);
                message.brand_key = Some(brand);
                message.occurred_at = occurred_at;
                let saved = ingest_inbound(&db, &message).await.unwrap();
                if i < 2 {
                    tied.push(saved.conversation_id);
                } else if i == 2 {
                    oldest = Some(saved.conversation_id);
                } else if i == 5 {
                    newest = Some(saved.conversation_id);
                }
            }
            tied.sort_by_key(|id| std::cmp::Reverse(*id));
            tied.insert(0, newest.unwrap());
            tied.push(oldest.unwrap());
            let filters = IntakeFilters {
                brand: Some("neon"),
                channel: Some(channel::EMAIL),
                direction: Some(direction::INBOUND),
                unread_only: true,
                ..Default::default()
            };
            let mut after = None;
            for expected_id in tied {
                let page = intake_page(&db, world.lawyer, filters, after, 1).await.unwrap();
                assert_eq!(page.rows.iter().map(|row| row.id).collect::<Vec<_>>(), [expected_id]);
                after = page.next;
            }
            assert!(after.is_none());
            assert!(matches!(
                intake_page(&db, world.client, filters, None, 1).await,
                Err(InboxError::Denied)
            ));
        });
    }

    #[tokio::test]
    async fn brand_scoping_keeps_the_firm_scope_when_narrowing() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut branded = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-vesta-1",
            "vesta-sender@example.com",
        );
        branded.brand_key = Some("vesta");
        let firm_vesta = ingest_inbound(&db, &branded).await.unwrap();
        let mut other_branded = inbound(
            world.other_firm,
            channel::EMAIL,
            "support",
            "msg-vesta-2",
            "other-sender@example.com",
        );
        other_branded.brand_key = Some("vesta");
        let other_firm_vesta = ingest_inbound(&db, &other_branded).await.unwrap();
        let mut firm_neon = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-neon-1",
            "neon-sender@example.com",
        );
        firm_neon.brand_key = Some("neon");
        ingest_inbound(&db, &firm_neon).await.unwrap();
        let mut firm_other_brand = inbound(
            world.other_firm,
            channel::EMAIL,
            "support",
            "msg-ob-1",
            "ob-sender@example.com",
        );
        firm_other_brand.brand_key = Some("otherfirm-brand");
        ingest_inbound(&db, &firm_other_brand).await.unwrap();

        // The Owner reads across Firms: one Brand view carries both Firms'
        // rows, and the brand vocabulary is everything visible.
        let scoped = list_intake_in_brand(&db, world.owner, "vesta")
            .await
            .unwrap();
        assert_eq!(scoped.len(), 2);
        assert!(scoped
            .iter()
            .any(|row| row.id == firm_vesta.conversation_id));
        assert!(scoped
            .iter()
            .any(|row| row.id == other_firm_vesta.conversation_id));
        assert_eq!(
            intake_brands(&db, world.owner).await.unwrap(),
            vec![
                "neon".to_string(),
                "otherfirm-brand".to_string(),
                "vesta".to_string()
            ]
        );

        // The firm member sees the same Brand narrowed to their Firms, and
        // never a Brand that only exists in a Firm they cannot see.
        let scoped = list_intake_in_brand(&db, world.lawyer, "vesta")
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, firm_vesta.conversation_id);
        assert_eq!(scoped[0].firm_id, world.firm);
        assert_eq!(
            intake_brands(&db, world.lawyer).await.unwrap(),
            vec!["neon".to_string(), "vesta".to_string()]
        );

        // A Client reads the matter log, not the inbox — brand views included.
        assert!(matches!(
            list_intake_in_brand(&db, world.client, "vesta")
                .await
                .map(|_| ())
                .unwrap_err(),
            InboxError::Denied
        ));
        assert!(matches!(
            intake_brands(&db, world.client).await.unwrap_err(),
            InboxError::Denied
        ));

        });
    }

    #[tokio::test]
    async fn sender_identity_resolves_each_channel_through_its_address_book() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;

        // Email resolves by the address a Person records — and an unknown
        // sender grows the CRM: the ingest itself created Pat's Person, so
        // the lookup (not a create) names it here.
        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-id-1",
                "pat.sender@example.com",
            ),
        )
        .await
        .unwrap();
        let pat = persons::find_by_any_email(&db, "pat.sender@example.com")
            .await
            .unwrap()
            .expect("the ingest auto-created the unknown sender's Person")
            .id;
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == email.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(pat));

        // SMS keeps resolving by the phone a Person records.
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "account",
                "sms-id-1",
                "+15555550123",
            ),
        )
        .await
        .unwrap();
        let sam = person(
            &db,
            "synthetic contact",
            "sam.texter@example.com",
            Role::Client,
        )
        .await;
        link_contact_phone(
            &db,
            world.owner,
            text.conversation_id,
            "sam.texter@example.com",
        )
        .await
        .unwrap();
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == text.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(sam));

        // A social channel resolves through the identity address book keyed
        // on the provider id the webhook recorded — and until that link
        // exists, the row shows the bare id, not a guess.
        let social = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::FACEBOOK,
                "page-account",
                "fb-msg-1",
                "fb-psid-123",
            ),
        )
        .await
        .unwrap();
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == social.conversation_id)
            .unwrap();
        assert_eq!(row.contact, None);

        external_identities::link(
            &db,
            pat,
            external_identities::ExternalSystem::Facebook,
            "fb-psid-123",
            Some("Pat on Facebook"),
        )
        .await
        .unwrap();

        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == social.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(pat));
        assert_eq!(
            row.contact.as_ref().map(|c| c.role.as_str()),
            Some("client")
        );

        });
    }

    #[test]
    fn a_preview_collapses_whitespace_and_ellipsizes_past_the_limit() {
        assert_eq!(
            preview_snippet("  Multi\nline \t body  ").as_deref(),
            Some("Multi line body")
        );
        let long = "x".repeat(PREVIEW_CHAR_LIMIT + 5);
        let snippet = preview_snippet(&long).unwrap();
        assert_eq!(snippet.chars().count(), PREVIEW_CHAR_LIMIT + 1);
        assert!(snippet.ends_with('\u{2026}'));
        assert_eq!(preview_snippet("   \n\t ").as_deref(), None);
        assert_eq!(preview_snippet("").as_deref(), None);
    }

    #[tokio::test]
    async fn rows_carry_the_latest_message_preview_and_the_persisted_person() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut first_message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-preview-1",
            "client@example.com",
        );
        first_message.provider_thread_ref = Some("email-preview-thread");
        let first = ingest_inbound(&db, &first_message).await.unwrap();
        // A second message on the same conversation moves the preview to it.
        let mut reply = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-preview-2",
            "client@example.com",
        );
        reply.body = "A later reply.";
        reply.occurred_at = "2026-10-01T12:00:00Z";
        reply.provider_thread_ref = Some("email-preview-thread");
        let _ = ingest_inbound(&db, &reply).await.unwrap();

        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == first.conversation_id)
            .unwrap();
        assert_eq!(row.preview.as_deref(), Some("A later reply."));
        assert_eq!(
            row.email_sent_at.unwrap().to_rfc3339(),
            "2026-10-01T12:00:00+00:00"
        );
        let created = persons::find_by_any_email(&db, "client@example.com")
            .await
            .unwrap()
            .expect("ingest resolved the sender to a Person");
        let person = row.person.as_ref().expect("the persisted Person badge");
        assert_eq!(person.person_id, created.id);
        // The fixture already holds a client-tier Person at this address, so
        // ingest resolved to it instead of creating one — the badge mirrors
        // the persisted Person's own name, not the message's display name.
        assert_eq!(person.name, created.name);

        // SMS ingests persist no association, so no badge — even when the
        // phone still resolves a contact at view time.
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "account",
                "sms-preview-1",
                "+15555550123",
            ),
        )
        .await
        .unwrap();
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == text.conversation_id)
            .unwrap();
        assert_eq!(row.person, None);
        assert_eq!(row.preview.as_deref(), Some("Hello from outside."));

        });
    }

    #[tokio::test]
    async fn an_acknowledgment_neither_previews_nor_counts_as_sent() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "fixture-account",
                "sms-ack-1",
                "sms:fixture-sender",
            ),
        )
        .await
        .unwrap();
        let conversation_id = ingested.conversation_id;
        assign(&db, world.owner, conversation_id, world.project)
            .await
            .unwrap();
        reserve_acknowledgment(&db, &ingested, "We received your message.")
            .await
            .unwrap()
            .expect("the acknowledgment is reserved");
        let row = |rows: Vec<IntakeRow>| {
            rows.into_iter()
                .find(|row| row.id == conversation_id)
                .unwrap()
        };
        let listed = row(list_intake(&db, world.owner).await.unwrap());
        assert_eq!(listed.preview.as_deref(), Some("Hello from outside."));
        assert!(!listed.sent);

        queue_reply(
            &db,
            world.lawyer,
            conversation_id,
            "A lawyer wrote this.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let listed = row(list_intake(&db, world.owner).await.unwrap());
        assert_eq!(listed.preview.as_deref(), Some("A lawyer wrote this."));
        assert!(listed.sent);

        });
    }

    #[tokio::test]
    async fn a_sender_identity_never_satisfies_a_dispatch_or_privilege_check() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut incoming = inbound(
            world.firm,
            channel::FACEBOOK,
            "page-account",
            "fb-msg-9",
            "fb-psid-9",
        );
        incoming.interaction_type = Some("comment");
        incoming.visibility = Some("public");
        let saved = ingest_inbound(&db, &incoming).await.unwrap();
        let sender = person(
            &db,
            "synthetic sender",
            "identity-sender@example.com",
            Role::Client,
        )
        .await;

        // Before the link exists: a Lawyer with no participation on the
        // conversation cannot queue a social reply.
        assert!(
            queue_social_reply(&db, world.lawyer, saved.conversation_id, "Reply", "public")
                .await
                .is_err()
        );

        // The address book learns who the sender is...
        external_identities::link(
            &db,
            sender,
            external_identities::ExternalSystem::Facebook,
            "fb-psid-9",
            None,
        )
        .await
        .unwrap();

        // ...and nothing changes about who may act: the link is display
        // data, not authority.
        assert!(
            queue_social_reply(&db, world.lawyer, saved.conversation_id, "Reply", "public")
                .await
                .is_err()
        );
        assert!(
            queue_social_reply(&db, world.owner, saved.conversation_id, "Reply", "public")
                .await
                .is_ok()
        );

        // The privilege rule layer never consults the address book either:
        // the social channel's structural rule stands with the identity
        // present, and no classifier is admitted for it.
        assert_eq!(
            crate::inbox_triage::privilege_label(channel::FACEBOOK, true, None),
            crate::inbox_triage::PrivilegeLabel::NotPrivileged
        );
        assert!(!crate::inbox_triage::classifier_may_run(channel::FACEBOOK));

        });
    }

    #[tokio::test]
    async fn a_text_shows_which_person_owns_the_number_once_it_is_linked() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "+15550100000",
                "sms-link-1",
                "+15555550123",
            ),
        )
        .await
        .unwrap();
        let unlinked = intake_detail(&db, world.owner, text.conversation_id)
            .await
            .unwrap();
        assert_eq!(unlinked.contact, None);

        let client_email = "pat.client@example.com";
        let pat = person(&db, "Pat Client", client_email, Role::Client).await;
        let alias = "pat.alias@example.com";
        persons::add_alias(&db, pat, alias).await.unwrap();
        let linked = link_contact_phone(&db, world.owner, text.conversation_id, alias)
            .await
            .unwrap();
        assert_eq!(linked.person_id, pat);
        assert_eq!(linked.role, "client");
        assert_eq!(
            persons::find_by_id(&db, pat).await.unwrap().unwrap().phone,
            Some("+15555550123".to_string())
        );
        assert_eq!(
            crate::person_phones::list_phones(&db, pat)
                .await
                .unwrap()
                .iter()
                .map(|phone| phone.e164.as_str())
                .collect::<Vec<_>>(),
            ["+15555550123"]
        );
        assert_eq!(
            conversation_by_id(&db, text.conversation_id)
                .await
                .unwrap()
                .unwrap()
                .person_id,
            Some(pat)
        );

        // The punctuation a person typed years ago still matches the E.164 number.
        let detail = intake_detail(&db, world.owner, text.conversation_id)
            .await
            .unwrap();
        assert_eq!(
            detail.contact.as_ref().map(|c| c.name.as_str()),
            Some("Pat Client")
        );
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == text.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(pat));

        // Linking again is idempotent; a second person cannot claim the number.
        link_contact_phone(&db, world.owner, text.conversation_id, client_email)
            .await
            .unwrap();
        person(&db, "Sam Other", "sam.other@example.com", Role::Client).await;
        assert!(matches!(
            link_contact_phone(
                &db,
                world.owner,
                text.conversation_id,
                "sam.other@example.com"
            )
            .await,
            Err(InboxError::Contact(_))
        ));

        });
    }

    #[tokio::test]
    async fn linking_a_number_refuses_unknown_emails_other_phones_and_outsiders() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "+15550100000",
                "sms-link-2",
                "+15555550199",
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            link_contact_phone(&db, world.owner, text.conversation_id, "nobody@example.com").await,
            Err(InboxError::Contact(_))
        ));
        let has_phone = person(&db, "Has Phone", "has.phone@example.com", Role::Client).await;
        persons::set_phone(&db, has_phone, Some("(555) 555-0000"))
            .await
            .unwrap();
        link_contact_phone(
            &db,
            world.owner,
            text.conversation_id,
            "has.phone@example.com",
        )
        .await
        .unwrap();
        let saved = persons::find_by_id(&db, has_phone).await.unwrap().unwrap();
        assert_eq!(saved.phone.as_deref(), Some("(555) 555-0000"));
        let phones = crate::person_phones::list_phones(&db, has_phone)
            .await
            .unwrap();
        assert_eq!(phones.len(), 2);
        assert!(phones.iter().any(|phone| phone.e164 == "+15555550199"));
        assert!(matches!(
            link_contact_phone(
                &db,
                world.other_admin,
                text.conversation_id,
                "has.phone@example.com"
            )
            .await,
            Err(InboxError::Denied)
        ));
        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-3",
                "a@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            link_contact_phone(
                &db,
                world.owner,
                email.conversation_id,
                "has.phone@example.com"
            )
            .await,
            Err(InboxError::Denied)
        ));

        });
    }

    #[tokio::test]
    async fn an_email_conversation_refuses_a_text_reply() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-2",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            queue_sms_reply(&db, world.owner, email.conversation_id, "Hi").await,
            Err(InboxError::Denied)
        ));

        });
    }

    #[tokio::test]
    async fn social_reply_allows_owner_and_own_firm_admin_without_weakening_matter_replies() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        firms::ensure_membership(
            &db,
            &NewPersonFirmRole {
                person_id: world.admin,
                firm_id: world.firm,
                membership: FirmMembership::Admin,
                is_dri: true,
            },
        )
        .await
        .unwrap();
        let brand_key = "social-reply-brand";
        crate::brands::create(
            &db,
            Role::Admin,
            Some(world.admin),
            &crate::brands::NewBrand {
                name: "Social Reply Brand".into(),
                key: brand_key.into(),
                firm_id: Some(world.firm),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let mut social_inbound = inbound(
            world.firm,
            channel::FACEBOOK,
            "page-1",
            "comment-1",
            "sender-1",
        );
        social_inbound.brand_key = Some(brand_key);
        social_inbound.interaction_type = Some("comment");
        social_inbound.provider_post_id = Some("page-post-1");
        social_inbound.provider_thread_ref = Some("comment-thread-1");
        social_inbound.visibility = Some("public");
        let received = ingest_inbound(&db, &social_inbound).await.unwrap();
        let admin_detail = intake_detail(&db, world.admin, received.conversation_id)
            .await
            .unwrap();
        assert!(admin_detail.bodies_visible);
        assert!(admin_detail.can_social_reply);
        assert_eq!(admin_detail.messages.len(), 1);

        for (actor, body) in [
            (world.owner, "Owner reply"),
            (world.admin, "Firm Admin reply"),
        ] {
            queue_social_reply(&db, actor, received.conversation_id, body, "public")
                .await
                .unwrap();
        }
        assert!(queue_social_reply(
            &db,
            world.other_admin,
            received.conversation_id,
            "Cross-Firm reply",
            "public",
        )
        .await
        .is_err());

        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "social-reply-1".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.submitted, 2);
        let calls = transport.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| {
            call.provider_post_id.as_deref() == Some("page-post-1")
                && call.reply_to_provider_message_id.as_deref() == Some("comment-1")
                && call.provider_thread_ref.as_deref() == Some("comment-thread-1")
        }));

        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(queue_reply(
            &db,
            world.admin,
            email.conversation_id,
            "Admin cannot send a matter email",
            purpose::MATTER,
        )
        .await
        .is_err());

        });
    }

    #[tokio::test]
    async fn a_direct_message_cannot_queue_a_public_reply() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
        let world = world(&db).await;
        let mut incoming = inbound(world.firm, channel::INSTAGRAM, "account", "dm-1", "sender");
        incoming.interaction_type = Some("direct_message");
        incoming.visibility = Some("private");
        let saved = ingest_inbound(&db, &incoming).await.unwrap();
        assert!(
            queue_social_reply(&db, world.owner, saved.conversation_id, "Reply", "public")
                .await
                .is_err()
        );
        assert_eq!(
            crate::postgres::inbox::messages_for_on(&pg, saved.conversation_id)
                .await
                .unwrap()
                .len(),
            1
        );
        queue_social_reply(&db, world.owner, saved.conversation_id, "Reply", "private")
            .await
            .unwrap();
        });
    }

    #[tokio::test]
    async fn comments_and_direct_messages_have_separate_transcripts() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut comment = inbound(
            world.firm,
            channel::INSTAGRAM,
            "account",
            "comment-id",
            "sender",
        );
        comment.interaction_type = Some("comment");
        comment.visibility = Some("public");
        let public = ingest_inbound(&db, &comment).await.unwrap();
        let mut dm = comment;
        dm.interaction_type = Some("direct_message");
        dm.visibility = Some("private");
        dm.provider_message_id = "private-id";
        let private = ingest_inbound(&db, &dm).await.unwrap();
        assert_ne!(public.conversation_id, private.conversation_id);

        });
    }

    #[tokio::test]
    async fn private_history_blocks_both_queueing_and_previously_queued_public_sends() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut incoming = inbound(
            world.firm,
            channel::INSTAGRAM,
            "account",
            "comment-id",
            "sender",
        );
        incoming.interaction_type = Some("comment");
        incoming.visibility = Some("public");
        let saved = ingest_inbound(&db, &incoming).await.unwrap();
        queue_social_reply(
            &db,
            world.owner,
            saved.conversation_id,
            "Synthetic reply",
            "public",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE message SET visibility = 'private' WHERE id = $1")
            .bind(saved.message_id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(queue_social_reply(
            &db,
            world.owner,
            saved.conversation_id,
            "Synthetic reply",
            "public"
        )
        .await
        .is_err());
        let transport = RecordingTransport::new(TransportOutcome::Unknown);
        assert_eq!(
            dispatch_queued(&db, &transport).await.unwrap().suppressed,
            1
        );
        assert!(transport.calls().is_empty());

        });
    }

    #[tokio::test]
    async fn a_later_callback_can_deliver_unknown_without_regressing() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        let reply = queue_reply(
            &db,
            world.clerk,
            conversation_id,
            "Checking status.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        crate::postgres::inbox::set_delivery_on(&pg, reply, delivery::UNKNOWN, Some("provider-9"))
            .await
            .unwrap();
        apply_delivery(
            &db,
            world.other_firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::DELIVERED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::UNKNOWN,
            "another firm's callback must not settle this delivery"
        );
        apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::SUBMITTED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::UNKNOWN,
            "an earlier submitted callback must not clear unknown"
        );
        apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::DELIVERED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::DELIVERED
        );
        apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::SUBMITTED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::DELIVERED,
            "a late callback must not regress delivery"
        );
        });
    }

    #[tokio::test]
    async fn attachment_access_follows_the_reader() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let asset_id = Uuid::now_v7();
        sqlx::query("INSERT INTO asset (id, project_id, storage_key, content_type, byte_size, sha256_hex) VALUES ($1, $2, 'synthetic/attachment-access', 'application/pdf', 1, 'sha-reader')")
            .bind(asset_id)
            .bind(world.project)
            .execute(pg.pool())
            .await
            .unwrap();
        let mut inbound = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-1",
            "sender@example.com",
        );
        inbound.attachments = vec![attachment(asset_id, "complaint.pdf", "sha-reader")];
        let ingested = ingest_inbound(&db, &inbound).await.unwrap();
        assert_eq!(
            readable_attachment(&db, world.owner, ingested.message_id)
                .await
                .unwrap(),
            Some(asset_id)
        );
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        assert!(readable_attachment(&db, world.owner, ingested.message_id)
            .await
            .is_err());
        assert_eq!(
            readable_attachment(&db, world.lawyer, ingested.message_id)
                .await
                .unwrap(),
            Some(asset_id)
        );
        assert!(readable_attachment(&db, world.client, ingested.message_id)
            .await
            .is_err());
        assert!(
            readable_attachment(&db, world.outsider, ingested.message_id)
                .await
                .is_err()
        );
        let note = post_note(
            &db,
            world.lawyer,
            ingested.conversation_id,
            "Internal file note.",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE message SET attachment_asset_id = $1 WHERE id = $2")
            .bind(asset_id)
            .bind(note)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(readable_attachment(&db, world.client, note).await.is_err());
        assert_eq!(
            readable_attachment(&db, world.clerk, note).await.unwrap(),
            Some(asset_id)
        );

        });
    }

    #[tokio::test]
    async fn assigned_intake_hides_bodies_from_an_owner_who_does_not_participate() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let open = intake_detail(&db, world.owner, ingested.conversation_id)
            .await
            .unwrap();
        assert!(open.bodies_visible);
        assert_eq!(open.messages.len(), 1);
        assert_eq!(open.next_action, "Assign to a project");
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let assigned = intake_detail(&db, world.owner, ingested.conversation_id)
            .await
            .unwrap();
        assert!(!assigned.bodies_visible);
        assert!(assigned.messages.is_empty());
        assert_eq!(assigned.next_action, "Reassign");
        let listed = list_intake(&db, world.owner).await.unwrap();
        assert_eq!(listed[0].assignee, "Alpha Matter");
        assert_eq!(listed[0].channel, channel::EMAIL);

        });
    }

    /// The whitelist stays closed: adding the voice lane admits no others.
    #[tokio::test]
    async fn an_unlisted_channel_is_still_denied() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let mut fax = inbound(world.firm, "fax", "+15550100000", "fax-1", "+15555550100");
        fax.brand_key = Some("neon");
        assert!(matches!(
            ingest_inbound(&db, &fax).await,
            Err(InboxError::Denied)
        ));

        });
    }

    #[test]
    fn delivery_does_not_move_backward() {
        assert_eq!(
            advance_delivery(delivery::DELIVERED, delivery::SUBMITTED),
            delivery::DELIVERED
        );
        assert_eq!(
            advance_delivery(delivery::UNKNOWN, delivery::DELIVERED),
            delivery::DELIVERED
        );
        assert_eq!(
            advance_delivery(delivery::UNKNOWN, delivery::SUBMITTED),
            delivery::UNKNOWN
        );
    }

    pub(super) async fn ingest_for(db: &ApplicationStore, firm: Uuid, provider_id: &str) -> Uuid {
        let message = inbound(
            firm,
            channel::EMAIL,
            "process-account",
            provider_id,
            "client@example.com",
        );
        ingest_inbound(db, &message).await.unwrap().message_id
    }

    #[tokio::test]
    async fn unprocessed_inbox_scopes_by_firm_membership() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let firm_message = ingest_for(&db, world.firm, "process-scope-1").await;
        let foreign_message = ingest_for(&db, world.other_firm, "process-scope-2").await;

        let owner_view = unprocessed_inbox(&db, world.owner).await.unwrap();
        let owner_ids: Vec<Uuid> = owner_view.iter().map(|row| row.id).collect();
        assert!(owner_ids.contains(&firm_message));
        assert!(owner_ids.contains(&foreign_message));

        let lawyer_view = unprocessed_inbox(&db, world.lawyer).await.unwrap();
        let lawyer_ids: Vec<Uuid> = lawyer_view.iter().map(|row| row.id).collect();
        assert!(lawyer_ids.contains(&firm_message));
        assert!(!lawyer_ids.contains(&foreign_message));

        assert!(matches!(
            unprocessed_inbox(&db, world.client).await,
            Err(InboxError::Denied)
        ));

        });
    }

    #[tokio::test]
    async fn unprocessed_inbox_orders_oldest_first() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let first = ingest_for(&db, world.firm, "process-order-1").await;
        let second = ingest_for(&db, world.firm, "process-order-2").await;
        sqlx::query("UPDATE message SET inserted_at = $1 WHERE id = $2")
            .bind(chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap())
            .bind(second)
            .execute(pg.pool())
            .await
            .unwrap();

        let listed = unprocessed_inbox(&db, world.lawyer).await.unwrap();
        let ids: Vec<Uuid> = listed.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![second, first]);

        });
    }

    #[tokio::test]
    async fn process_context_scopes_the_single_message_like_the_backlog() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        let firm_message = ingest_for(&db, world.firm, "process-ctx-1").await;
        let foreign_message = ingest_for(&db, world.other_firm, "process-ctx-2").await;
        mark_processed(&db, firm_message, route::NEF, Utc::now(), None)
            .await
            .unwrap();

        // The Owner sees any firm's message, with its recorded state.
        let owner = process_context(&db, world.owner, firm_message)
            .await
            .unwrap();
        assert_eq!(owner.state.route.as_deref(), Some(route::NEF));
        assert!(!owner.row.body.is_empty());
        assert_eq!(owner.row.external_address, "client@example.com");

        // A firm member sees their own firm's message.
        let member = process_context(&db, world.lawyer, firm_message)
            .await
            .unwrap();
        assert_eq!(member.row.id, firm_message);

        // Another firm's message is not found, never disclosed.
        assert!(matches!(
            process_context(&db, world.lawyer, foreign_message).await,
            Err(InboxError::NotFound)
        ));

        // A client-tier actor is denied.
        assert!(matches!(
            process_context(&db, world.client, firm_message).await,
            Err(InboxError::Denied)
        ));

        // A missing message is not found.
        assert!(matches!(
            process_context(&db, world.owner, Uuid::now_v7()).await,
            Err(InboxError::NotFound)
        ));

        });
    }
    /// An inbound message's acknowledgment receipt is its ID with the
    /// sign bit flipped: always distinct from the inbound ID, and
    /// involutive, so a reply to the receipt resolves to the original.
    #[test]
    fn acknowledgment_ids_are_involutive_and_distinct() {
        let inbound = Uuid::now_v7();
        let receipt = acknowledgment_id(inbound);
        assert_ne!(inbound, receipt);
        assert_eq!(acknowledgment_id(receipt), inbound);
    }

    #[test]
    fn e164_requires_a_plus_an_eligible_length_and_no_leading_zero() {
        assert!(is_e164("+15551234567"));
        assert!(is_e164("+33123456789"));
        // Too short, too long, missing `+`, a leading zero, non-digits.
        assert!(!is_e164("+1555123"));
        assert!(!is_e164("+1555123456789012345"));
        assert!(!is_e164("15551234567"));
        assert!(!is_e164("+05551234567"));
        assert!(!is_e164("+1555-1234567"));
        assert!(!is_e164(""));
    }

    #[test]
    fn social_channels_exclude_the_direct_channels() {
        for name in [
            channel::FACEBOOK,
            channel::INSTAGRAM,
            channel::THREADS,
            channel::YOUTUBE,
            channel::TIKTOK,
            channel::X,
            channel::LINKEDIN,
        ] {
            assert!(is_social_channel(name), "{name}");
        }
        for name in [
            channel::SMS,
            channel::VOICE,
            channel::EMAIL,
            channel::SCRAPER,
        ] {
            assert!(!is_social_channel(name), "{name}");
        }
    }

    #[test]
    fn terminal_delivery_states_stay_terminal_and_the_last_signal_wins() {
        use super::delivery;
        // A terminal state never regresses, whatever arrives later.
        assert_eq!(
            advance_delivery(delivery::FAILED, delivery::DELIVERED),
            delivery::FAILED
        );
        assert_eq!(
            advance_delivery(delivery::SUPPRESSED, delivery::SUBMITTED),
            delivery::SUPPRESSED
        );
        // A non-terminal state adopts the incoming signal, unknown stays
        // unknown while only a submit is known, and an unrecognized signal
        // reads as unknown rather than inventing a state.
        assert_eq!(
            advance_delivery(delivery::SUBMITTED, "carrier-pigeon"),
            delivery::UNKNOWN
        );
        assert_eq!(
            advance_delivery(delivery::RECEIVED, delivery::FAILED),
            delivery::FAILED
        );
    }

    #[tokio::test]
    async fn unread_count_for_project_counts_only_that_matters_unread_threads() {
        #[rustfmt::skip]
        with_native_inbox_fixture!(db, pg, {
let world = world(&db).await;
        // Two unread threads on the matter, one elsewhere, and the count
        // read stays a bare-number decode (SELECT VALUE count()).
        for thread in ["sms-a", "sms-b"] {
            let ingested = ingest_inbound(
                &db,
                &inbound(
                    world.firm,
                    channel::SMS,
                    "fixture-account",
                    thread,
                    "sms:fixture-sender",
                ),
            )
            .await
            .unwrap();
            assign(&db, world.owner, ingested.conversation_id, world.project)
                .await
                .unwrap();
        }
        assert_eq!(
            unread_count_for_project(&db, world.project).await.unwrap(),
            2
        );
        assert_eq!(
            unread_count_for_project(&db, world.other_project)
                .await
                .unwrap(),
            0
        );

        });
    }
}
