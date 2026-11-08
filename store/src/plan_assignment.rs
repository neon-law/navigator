//! Read a legal-plan assignment notice from a plan provider (ARAG,
//! LegalShield) — the email a fixed-fee plan sends when it assigns one of
//! its members to the firm.
//!
//! This module is pure: text in, an [`Assignment`] out. It opens no
//! connection and touches no file, so the intake lane in `portal` and any
//! offline import parse one assignment identically. Detection is
//! sender-based and conservative: a message from a plan provider that does
//! not parse is not silently dropped — it falls to triage for a person.

use chrono::NaiveDate;
use serde::Serialize;

/// The plan provider that sent the assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PlanProvider {
    /// ARAG legal plan.
    Arag,
    /// LegalShield legal plan.
    LegalShield,
}

impl PlanProvider {
    /// The storage spelling recorded beside the parsed assignment.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Arag => "arag",
            Self::LegalShield => "legal_shield",
        }
    }

    /// The provider's sender domain. Routing is sender-based, so a
    /// lookalike body from an unknown sender never parses.
    #[must_use]
    pub const fn sender_domain(self) -> &'static str {
        match self {
            Self::Arag => "arag.com",
            Self::LegalShield => "legalshield.com",
        }
    }

    /// The body marker every assignment notice from this provider carries.
    #[must_use]
    pub const fn body_marker(self) -> &'static str {
        match self {
            Self::Arag => "attorney assignment",
            Self::LegalShield => "legalshield",
        }
    }
}

/// The provider a sender address belongs to. The sender's domain is the
/// provider's domain or a subdomain of it — the same suffix rule the
/// claims-agent vendor route uses.
#[must_use]
pub fn provider_for_sender(from: &str) -> Option<PlanProvider> {
    let address = from
        .rsplit_once('@')?
        .1
        .trim_end_matches('>')
        .to_ascii_lowercase();
    [PlanProvider::Arag, PlanProvider::LegalShield]
        .into_iter()
        .find(|provider| {
            let suffix = provider.sender_domain();
            address == suffix || address.ends_with(&format!(".{suffix}"))
        })
}

/// One plan assignment as the notice describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    /// The provider that sent the notice.
    pub provider: PlanProvider,
    /// The member's name as the notice prints it.
    pub member_name: String,
    /// The provider's member or membership identifier.
    pub member_number: String,
    /// The plan or certificate number, when the notice prints one.
    pub plan_number: Option<String>,
    /// The kind of legal matter the plan assigned, in the notice's words.
    pub matter_type: String,
    /// The coverage/assignment effective date, when the notice prints one.
    pub effective_date: Option<NaiveDate>,
    /// The member's contact email, when the notice prints one.
    pub contact_email: Option<String>,
}

/// Why a message is not an assignment notice this module can read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanAssignmentError {
    #[error("not a legal-plan assignment notice")]
    NotAnAssignment,
    #[error("the assignment notice has no `{0}` line")]
    Missing(&'static str),
}

/// Parse the body text of an assignment notice.
///
/// # Errors
///
/// [`PlanAssignmentError::NotAnAssignment`] when the text carries no plan
/// provider's marker; [`PlanAssignmentError::Missing`] when a recognized
/// notice lacks a required field.
pub fn parse_text(text: &str) -> Result<Assignment, PlanAssignmentError> {
    let lowered = text.to_ascii_lowercase();
    let provider = [PlanProvider::Arag, PlanProvider::LegalShield]
        .into_iter()
        .find(|provider| lowered.contains(provider.body_marker()))
        .ok_or(PlanAssignmentError::NotAnAssignment)?;
    parse_for_provider(provider, text)
}

/// Parse the body of a notice from a sender already resolved to `provider`.
///
/// # Errors
///
/// [`PlanAssignmentError::Missing`] when the notice lacks a required field.
pub fn parse_for_provider(
    provider: PlanProvider,
    text: &str,
) -> Result<Assignment, PlanAssignmentError> {
    let fields = FieldLabels::for_provider(provider);
    let member_name =
        field(text, fields.member_name).ok_or(PlanAssignmentError::Missing("member name"))?;
    let member_number =
        field(text, fields.member_number).ok_or(PlanAssignmentError::Missing("member number"))?;
    let matter_type =
        field(text, fields.matter_type).ok_or(PlanAssignmentError::Missing("matter type"))?;
    Ok(Assignment {
        provider,
        member_name,
        member_number,
        plan_number: optional_field(text, fields.plan_number),
        matter_type,
        effective_date: optional_field(text, fields.effective_date).and_then(|value| {
            NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .or_else(|_| NaiveDate::parse_from_str(&value, "%m/%d/%Y"))
                .ok()
        }),
        contact_email: optional_field(text, fields.contact_email)
            .map(|value| value.to_ascii_lowercase()),
    })
}

/// The field labels one provider prints. Kept beside the parser so a new
/// provider is one struct literal, not a new match arm.
struct FieldLabels {
    member_name: &'static str,
    member_number: &'static str,
    plan_number: Option<&'static str>,
    matter_type: &'static str,
    effective_date: Option<&'static str>,
    contact_email: Option<&'static str>,
}

impl FieldLabels {
    const fn for_provider(provider: PlanProvider) -> Self {
        match provider {
            PlanProvider::Arag => Self {
                member_name: "member name",
                member_number: "member number",
                plan_number: Some("plan number"),
                matter_type: "matter type",
                effective_date: Some("effective date"),
                contact_email: Some("member email"),
            },
            PlanProvider::LegalShield => Self {
                member_name: "member",
                member_number: "membership id",
                plan_number: Some("certificate number"),
                matter_type: "legal matter",
                effective_date: Some("coverage start"),
                contact_email: Some("contact"),
            },
        }
    }
}

/// The value of an optional `Label: value` line, when the provider prints one.
fn optional_field(text: &str, label: Option<&str>) -> Option<String> {
    label.and_then(|label| field(text, label))
}

/// The value of a `Label: value` line, whitespace collapsed. Labels are
/// matched case-insensitively at the start of a line; the first match wins.
fn field(text: &str, label: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().to_ascii_lowercase() == label {
            let value = value.trim();
            (!value.is_empty()).then(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
        } else {
            None
        }
    })
}

/// Synthetic fixture emails for tests and offline imports: reserved
/// example.com addresses, 555-prefix phones, no real member. The portal's
/// intake lane tests parse the same fixtures the store's unit tests do, so
/// both lanes read one assignment identically.
pub mod fixtures {

    /// The ARAG fixture's sender address.
    pub const ARAG_SENDER: &str = "assignments@notice.arag.example";
    /// The LegalShield fixture's sender address.
    pub const LEGAL_SHIELD_SENDER: &str = "assignments@notice.legalshield.example";

    /// A synthetic ARAG attorney-assignment notice.
    #[must_use]
    pub fn arag_assignment_text() -> String {
        [
            "ARAG Attorney Assignment",
            "",
            "Member Name: Dana Example",
            "Member Number: AR-004512",
            "Plan Number: PLAN-778812",
            "Matter Type: Estate Planning - Will Preparation",
            "Effective Date: 2026-11-02",
            "Member Email: dana@example.com",
            "",
            "Please contact the member within two business days to schedule",
            "an initial consultation. Reply to this notice if you must decline",
            "the assignment.",
        ]
        .join("\n")
    }

    /// A synthetic LegalShield membership-assignment notice.
    #[must_use]
    pub fn legal_shield_assignment_text() -> String {
        [
            "LegalShield Member Assignment",
            "",
            "Member: Riley Example",
            "Membership ID: LS-902441",
            "Certificate Number: CERT-330291",
            "Legal Matter: Consumer Contract Review",
            "Coverage Start: 11/04/2026",
            "Contact: riley@example.com",
            "",
            "You have been assigned to this member's matter. Contact the",
            "member to schedule the covered consultation.",
        ]
        .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fixtures, parse_for_provider, parse_text, provider_for_sender, PlanAssignmentError,
        PlanProvider,
    };

    #[test]
    fn the_arag_fixture_parses_with_its_fields_intact() {
        let assignment = parse_text(&fixtures::arag_assignment_text()).expect("fixture parses");
        assert_eq!(assignment.provider, PlanProvider::Arag);
        assert_eq!(assignment.member_name, "Dana Example");
        assert_eq!(assignment.member_number, "AR-004512");
        assert_eq!(assignment.plan_number.as_deref(), Some("PLAN-778812"));
        assert_eq!(assignment.matter_type, "Estate Planning - Will Preparation");
        assert_eq!(assignment.effective_date.unwrap().to_string(), "2026-11-02");
        assert_eq!(
            assignment.contact_email.as_deref(),
            Some("dana@example.com")
        );
    }

    #[test]
    fn the_legal_shield_fixture_parses_with_its_fields_intact() {
        let assignment =
            parse_text(&fixtures::legal_shield_assignment_text()).expect("fixture parses");
        assert_eq!(assignment.provider, PlanProvider::LegalShield);
        assert_eq!(assignment.member_name, "Riley Example");
        assert_eq!(assignment.member_number, "LS-902441");
        assert_eq!(assignment.plan_number.as_deref(), Some("CERT-330291"));
        assert_eq!(assignment.matter_type, "Consumer Contract Review");
        assert_eq!(assignment.effective_date.unwrap().to_string(), "2026-11-04");
        assert_eq!(
            assignment.contact_email.as_deref(),
            Some("riley@example.com")
        );
    }

    #[test]
    fn provider_spellings_are_stable() {
        assert_eq!(PlanProvider::Arag.as_str(), "arag");
        assert_eq!(PlanProvider::LegalShield.as_str(), "legal_shield");
    }

    #[test]
    fn provider_sender_domains_are_the_noticing_vendors() {
        assert_eq!(PlanProvider::Arag.sender_domain(), "arag.com");
        assert_eq!(PlanProvider::LegalShield.sender_domain(), "legalshield.com");
    }

    #[test]
    fn text_without_a_provider_marker_is_not_an_assignment() {
        assert_eq!(
            parse_text("Dear counsel, please advise."),
            Err(PlanAssignmentError::NotAnAssignment)
        );
    }

    #[test]
    fn a_recognized_notice_without_its_member_number_is_missing_not_unparseable() {
        let text = fixtures::arag_assignment_text()
            .lines()
            .filter(|line| !line.starts_with("Member Number:"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_text(&text),
            Err(PlanAssignmentError::Missing("member number"))
        );
    }

    #[test]
    fn a_recognized_notice_without_a_matter_type_is_missing_not_unparseable() {
        let text = fixtures::legal_shield_assignment_text()
            .lines()
            .filter(|line| !line.starts_with("Legal Matter:"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_text(&text),
            Err(PlanAssignmentError::Missing("matter type"))
        );
    }

    #[test]
    fn labels_are_case_insensitive() {
        let text = fixtures::arag_assignment_text().replace("Member Name:", "MEMBER NAME:");
        let assignment = parse_text(&text).expect("case-insensitive labels parse");
        assert_eq!(assignment.member_name, "Dana Example");
    }

    #[test]
    fn crlf_line_endings_and_padding_parse() {
        let text = fixtures::arag_assignment_text().replace('\n', "\r\n");
        let assignment = parse_text(&text).expect("crlf parses");
        assert_eq!(assignment.member_name, "Dana Example");
    }

    #[test]
    fn an_optional_field_may_be_absent() {
        let text = fixtures::arag_assignment_text()
            .lines()
            .filter(|line| !line.starts_with("Effective Date:"))
            .collect::<Vec<_>>()
            .join("\n");
        let assignment = parse_for_provider(PlanProvider::Arag, &text).expect("parses");
        assert_eq!(assignment.effective_date, None);
    }

    #[test]
    fn an_unreadable_effective_date_is_none_not_an_error() {
        let text = fixtures::arag_assignment_text().replace("2026-11-02", "next Monday");
        let assignment = parse_for_provider(PlanProvider::Arag, &text).expect("parses");
        assert_eq!(assignment.effective_date, None);
    }

    #[test]
    fn parse_for_provider_skips_sender_detection() {
        // A body carrying no provider marker still parses when the lane has
        // already resolved the sender to a provider.
        let text = "Member Name: Dana Example\nMember Number: AR-004512\nMatter Type: Advice";
        let assignment = parse_for_provider(PlanProvider::Arag, text).expect("parses");
        assert_eq!(assignment.provider, PlanProvider::Arag);
        assert_eq!(assignment.contact_email, None);
    }

    #[test]
    fn provider_sender_routes_match_subdomains_only() {
        assert_eq!(
            provider_for_sender("ARAG Assignments <assignments@notice.arag.com>"),
            Some(PlanProvider::Arag)
        );
        assert_eq!(
            provider_for_sender("mail@legalshield.com"),
            Some(PlanProvider::LegalShield)
        );
        assert_eq!(provider_for_sender("spoof@notarag.com"), None);
        assert_eq!(provider_for_sender("someone@example.com"), None);
        assert_eq!(provider_for_sender("no-address"), None);
    }

    #[test]
    fn fixtures_use_reserved_synthetic_senders() {
        for sender in [fixtures::ARAG_SENDER, fixtures::LEGAL_SHIELD_SENDER] {
            assert!(sender.ends_with(".example"), "{sender} is not synthetic");
        }
    }
}
