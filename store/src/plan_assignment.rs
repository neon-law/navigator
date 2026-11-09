//! Read a legal-plan notice from a plan provider (ARAG, `LegalEASE`,
//! `LegalShield`) — the case request a fixed-fee plan sends when one of its
//! members asks for an attorney, and the messages that follow about that
//! case. Every one of them names the matter by the plan's own number, the
//! Plan Case Number.
//!
//! This module is pure: subject and text in, an [`Assignment`] out. It
//! opens no connection and touches no file, so the intake lane in `portal`
//! and any offline import parse one notice identically. Detection is
//! sender-based and conservative: a message from a plan provider that does
//! not parse is not silently dropped — it falls to triage for a person.
//!
//! The Plan Case Number is read from the subject first, because each
//! provider prints it there and a subject survives the trip through
//! `SendGrid` unchanged. Plan mail is HTML; when the inbound text is derived
//! from it, table cells and line breaks collapse into one line, so body
//! fields are read from whitespace-flattened text, never by line.

use chrono::NaiveDate;
use serde::Serialize;

/// The plan provider that sent the notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PlanProvider {
    /// ARAG legal plan: `CaseAssist` case requests and provider mail.
    Arag,
    /// `LegalEASE` legal plan: case reviews from LAMP and provider mail.
    LegalEase,
    /// `LegalShield` legal plan.
    LegalShield,
}

impl PlanProvider {
    const ALL: [Self; 3] = [Self::Arag, Self::LegalEase, Self::LegalShield];

    /// The storage spelling recorded beside the parsed notice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Arag => "arag",
            Self::LegalEase => "legal_ease",
            Self::LegalShield => "legal_shield",
        }
    }

    /// The domains the provider sends from. Routing is sender-based, so a
    /// lookalike body from an unknown sender never parses. ARAG sends case
    /// requests from `aragconnect.com` and provider mail from
    /// `araglegal.com`.
    #[must_use]
    pub const fn sender_domains(self) -> &'static [&'static str] {
        match self {
            Self::Arag => &["aragconnect.com", "araglegal.com", "arag.com"],
            Self::LegalEase => &["legaleaseplan.com"],
            Self::LegalShield => &["legalshield.com"],
        }
    }

    /// The phrases each provider prints just before its case number, in the
    /// subject or the body: `CaseAssist #4900000-001`, `Member Case #400001`,
    /// `Case ID: 400001`, a `| Case ID | 400001 |` table row.
    const fn case_markers(self) -> &'static [&'static str] {
        match self {
            Self::Arag => &["caseassist #", "caseassist"],
            Self::LegalEase => &["case #", "case id"],
            Self::LegalShield => &["certificate number"],
        }
    }
}

/// The provider a sender address belongs to. The sender's domain is one of
/// the provider's domains or a subdomain of one — the same suffix rule the
/// claims-agent vendor route uses.
#[must_use]
pub fn provider_for_sender(from: &str) -> Option<PlanProvider> {
    let address = from
        .rsplit_once('@')?
        .1
        .trim_end_matches('>')
        .to_ascii_lowercase();
    PlanProvider::ALL.into_iter().find(|provider| {
        provider
            .sender_domains()
            .iter()
            .any(|domain| address == *domain || address.ends_with(&format!(".{domain}")))
    })
}

/// One plan notice as the provider prints it. Only the Plan Case Number is
/// required: it is what links the mail to its Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    /// The provider that sent the notice.
    pub provider: PlanProvider,
    /// The plan's own number for this matter — the Project's
    /// `plan_case_number`.
    pub case_number: String,
    /// The member's name as the notice prints it.
    pub member_name: Option<String>,
    /// The provider's member or membership identifier.
    pub member_number: Option<String>,
    /// The kind of legal matter, in the notice's words.
    pub matter_type: Option<String>,
    /// The plan or coverage effective date, when the notice prints one.
    pub effective_date: Option<NaiveDate>,
    /// The member's contact email, when the notice prints one.
    pub contact_email: Option<String>,
}

/// Why a message from a plan provider is not a notice this module can read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanAssignmentError {
    #[error("the plan notice has no `{0}`")]
    Missing(&'static str),
}

/// Parse a notice from a sender already resolved to `provider`.
///
/// # Errors
///
/// [`PlanAssignmentError::Missing`] when neither the subject nor the body
/// carries the provider's case number.
pub fn parse(
    provider: PlanProvider,
    subject: &str,
    text: &str,
) -> Result<Assignment, PlanAssignmentError> {
    let body = flatten(text);
    let case_number = case_number(provider, subject)
        .or_else(|| case_number(provider, &body))
        .ok_or(PlanAssignmentError::Missing("case number"))?;
    let mut assignment = Assignment {
        provider,
        case_number,
        member_name: None,
        member_number: None,
        matter_type: None,
        effective_date: None,
        contact_email: None,
    };
    match provider {
        PlanProvider::Arag => {
            // "… local network attorneys that <member> has requested
            // attorney services in your area for <matter>."
            assignment.member_name = between(&body, "attorneys that ", " has requested");
            assignment.matter_type = between(&body, "in your area for ", ".");
        }
        PlanProvider::LegalEase => {
            // Subject: "… Member Case #400001: <member> - <matter>".
            if let Some((member, matter)) = subject
                .split_once(": ")
                .and_then(|(_, rest)| rest.split_once(" - "))
            {
                assignment.member_name = non_empty(member);
                assignment.matter_type = non_empty(matter);
            }
            assignment.member_number = token_after(&body, "member id");
            assignment.effective_date =
                token_after(&body, "plan effective date").and_then(|value| date(&value));
        }
        PlanProvider::LegalShield => {
            assignment.member_name = field(text, "member");
            assignment.member_number = field(text, "membership id");
            assignment.matter_type = field(text, "legal matter");
            assignment.effective_date =
                field(text, "coverage start").and_then(|value| date(&value));
            assignment.contact_email =
                field(text, "contact").map(|value| value.to_ascii_lowercase());
        }
    }
    Ok(assignment)
}

/// Whitespace collapsed to single spaces, so HTML-derived text and real
/// plain text read alike.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The first case number `provider` prints in `haystack`: a token holding a
/// digit right after one of its markers.
fn case_number(provider: PlanProvider, haystack: &str) -> Option<String> {
    provider
        .case_markers()
        .iter()
        .filter_map(|marker| token_after(haystack, marker))
        .find(|token| token.chars().any(|c| c.is_ascii_digit()))
}

/// The token after the first case-insensitive `label` in `haystack`,
/// skipping the separators a label/value pair is printed with (`:`, `#`,
/// `|`, spaces). A token runs over letters, digits, `-`, and `/`.
fn token_after(haystack: &str, label: &str) -> Option<String> {
    let start = haystack.to_ascii_lowercase().find(label)? + label.len();
    let token: String = haystack[start..]
        .trim_start_matches([' ', ':', '#', '|'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/'))
        .collect();
    (!token.is_empty()).then_some(token)
}

/// The text between the first `start` and the next `end` after it.
fn between(haystack: &str, start: &str, end: &str) -> Option<String> {
    let rest = &haystack[haystack.find(start)? + start.len()..];
    non_empty(&rest[..rest.find(end)?])
}

fn date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value, "%m/%d/%Y"))
        .ok()
}

/// The value of a `Label: value` line, whitespace collapsed. Labels are
/// matched case-insensitively at the start of a line; the first match wins.
fn field(text: &str, label: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case(label) {
            non_empty(&flatten(value))
        } else {
            None
        }
    })
}

/// Synthetic notices in each provider's shape for tests and offline
/// imports: invented members, `example.com` addresses, and case numbers no
/// plan issued. The portal's intake lane tests parse the same fixtures the
/// store's unit tests do, so both lanes read one notice identically.
pub mod fixtures {
    /// The subject of an ARAG case request.
    pub const ARAG_SUBJECT: &str = "New Case Request Alert - CaseAssist #4900000-001";

    /// The text of an ARAG case request, flattened the way HTML-only mail
    /// reaches the inbound lane.
    #[must_use]
    pub fn arag_case_request_text() -> String {
        [
            "Time-sensitive: Please let us know if you are interested in this case.",
            "Case Request Alert Counsel, We are notifying you and other local",
            "network attorneys that Dana Example has requested attorney services",
            "in your area for Will Preparation. Follow these steps: 1. Click on",
            "Case Details to learn more about the case.",
        ]
        .join("\n")
    }

    /// The subject of a `LegalEASE` case review.
    pub const LEGAL_EASE_SUBJECT: &str =
        "LegalEASE NEW CASE REVIEW for Member Case #400001: Harper Sample - Lease Agreement";

    /// The text of a `LegalEASE` case review: its Personal Details table.
    #[must_use]
    pub fn legal_ease_case_review_text() -> String {
        [
            "Your attorney services are being requested for member case.",
            "| Personal Details | |",
            "|---|---|",
            "| Case ID | 400001 |",
            "| Name | Harper Sample |",
            "| Member ID | 9900001 |",
            "| Plan Effective Date | 1/1/2025 |",
            "| Legal Matter State | NV |",
            "| Area of Law | Lease Agreement |",
        ]
        .join("\n")
    }

    /// A synthetic `LegalShield` membership-assignment notice.
    #[must_use]
    pub fn legal_shield_assignment_text() -> String {
        [
            "LegalShield Member Assignment",
            "",
            "Member: Harper Sample",
            "Membership ID: LS-902441",
            "Certificate Number: CERT-330291",
            "Legal Matter: Consumer Contract Review",
            "Coverage Start: 11/04/2026",
            "Contact: riley@example.com",
        ]
        .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures, parse, provider_for_sender, PlanAssignmentError, PlanProvider};

    #[test]
    fn an_arag_case_request_reads_its_caseassist_number_member_and_matter() {
        let notice = parse(
            PlanProvider::Arag,
            fixtures::ARAG_SUBJECT,
            &fixtures::arag_case_request_text(),
        )
        .expect("case request parses");
        assert_eq!(notice.case_number, "4900000-001");
        assert_eq!(notice.member_name.as_deref(), Some("Dana Example"));
        assert_eq!(notice.matter_type.as_deref(), Some("Will Preparation"));
        assert_eq!(notice.member_number, None);
    }

    #[test]
    fn an_arag_reminder_and_provider_mail_read_the_same_number() {
        let reminder = parse(
            PlanProvider::Arag,
            "REMINDER: Case Request Alert - CaseAssist #4900000-002",
            "We still need to hear from you about this case.",
        )
        .expect("reminder parses");
        assert_eq!(reminder.case_number, "4900000-002");
        assert_eq!(reminder.member_name, None);

        let letter = parse(
            PlanProvider::Arag,
            "Engagement letter",
            "Thank you. The signed letter for CaseAssist 4900000-003 is on file.",
        )
        .expect("a body citation parses");
        assert_eq!(letter.case_number, "4900000-003");
    }

    #[test]
    fn a_legal_ease_case_review_reads_its_subject_and_table() {
        let notice = parse(
            PlanProvider::LegalEase,
            fixtures::LEGAL_EASE_SUBJECT,
            &fixtures::legal_ease_case_review_text(),
        )
        .expect("case review parses");
        assert_eq!(notice.case_number, "400001");
        assert_eq!(notice.member_name.as_deref(), Some("Harper Sample"));
        assert_eq!(notice.matter_type.as_deref(), Some("Lease Agreement"));
        assert_eq!(notice.member_number.as_deref(), Some("9900001"));
        assert_eq!(notice.effective_date.unwrap().to_string(), "2025-01-01");
    }

    #[test]
    fn a_legal_ease_table_flattened_from_html_reads_the_same() {
        // HTML-only mail reaches the lane with its table cells run together.
        let flat = "Personal Details Case ID 400001 Name Harper Sample Member ID 9900001 \
                    Plan Effective Date 1/1/2025 Group Name Example Co";
        let notice = parse(PlanProvider::LegalEase, "Assignment", flat).expect("parses");
        assert_eq!(notice.case_number, "400001");
        assert_eq!(notice.member_number.as_deref(), Some("9900001"));
        assert_eq!(notice.effective_date.unwrap().to_string(), "2025-01-01");
    }

    #[test]
    fn legal_ease_provider_mail_reads_the_case_id_subjects() {
        for (subject, number) in [
            (
                "Message for LegalEASE Case #400002: HARPER SAMPLE - Tax",
                "400002",
            ),
            ("LegalEASE Referral (Flat Fee). Case ID: 400003", "400003"),
            ("Case ID:400004 Client Name:   Harper Sample", "400004"),
        ] {
            let notice = parse(PlanProvider::LegalEase, subject, "").expect(subject);
            assert_eq!(notice.case_number, number, "{subject}");
        }
    }

    #[test]
    fn the_legal_shield_notice_parses_under_its_own_labels() {
        let notice = parse(
            PlanProvider::LegalShield,
            "Member Assignment",
            &fixtures::legal_shield_assignment_text(),
        )
        .expect("notice parses");
        assert_eq!(notice.case_number, "CERT-330291");
        assert_eq!(notice.member_name.as_deref(), Some("Harper Sample"));
        assert_eq!(notice.member_number.as_deref(), Some("LS-902441"));
        assert_eq!(
            notice.matter_type.as_deref(),
            Some("Consumer Contract Review")
        );
        assert_eq!(notice.effective_date.unwrap().to_string(), "2026-11-04");
        assert_eq!(notice.contact_email.as_deref(), Some("riley@example.com"));
    }

    #[test]
    fn plan_mail_without_a_case_number_is_missing_it() {
        for provider in PlanProvider::ALL {
            assert_eq!(
                parse(
                    provider,
                    "ARAG member Dana Example",
                    "Please call us about the case."
                ),
                Err(PlanAssignmentError::Missing("case number")),
                "{provider:?}"
            );
        }
    }

    #[test]
    fn provider_spellings_are_stable() {
        assert_eq!(PlanProvider::Arag.as_str(), "arag");
        assert_eq!(PlanProvider::LegalEase.as_str(), "legal_ease");
        assert_eq!(PlanProvider::LegalShield.as_str(), "legal_shield");
    }

    #[test]
    fn provider_senders_are_the_domains_plan_mail_comes_from() {
        for (from, provider) in [
            (
                "CaseRequest <caserequest@aragconnect.com>",
                PlanProvider::Arag,
            ),
            ("provider-desk@araglegal.com", PlanProvider::Arag),
            ("assignments@notice.arag.com", PlanProvider::Arag),
            ("no-reply@legaleaseplan.com", PlanProvider::LegalEase),
            ("mail@legalshield.com", PlanProvider::LegalShield),
        ] {
            assert_eq!(provider_for_sender(from), Some(provider), "{from}");
        }
        assert_eq!(provider_for_sender("spoof@notarag.com"), None);
        assert_eq!(provider_for_sender("someone@example.com"), None);
        assert_eq!(provider_for_sender("no-address"), None);
    }
}
