use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a document reached a recipient. Closed: a certificate of service
/// states one of these, as the rules of court name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceMethod {
    Personal,
    Mail,
    CertifiedMail,
    Overnight,
    EService,
    ElectronicByAgreement,
}

impl ServiceMethod {
    /// Every method, in the order help and errors name them.
    pub const ALL: [Self; 6] = [
        Self::Personal,
        Self::Mail,
        Self::CertifiedMail,
        Self::Overnight,
        Self::EService,
        Self::ElectronicByAgreement,
    ];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Mail => "mail",
            Self::CertifiedMail => "certified_mail",
            Self::Overnight => "overnight",
            Self::EService => "e_service",
            Self::ElectronicByAgreement => "electronic_by_agreement",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value.trim())
    }

    /// The method as a certificate of service states it.
    #[must_use]
    pub const fn certificate_phrase(self) -> &'static str {
        match self {
            Self::Personal => "by personal delivery",
            Self::Mail => "by mail",
            Self::CertifiedMail => "by certified mail",
            Self::Overnight => "by overnight delivery",
            Self::EService => "by electronic service",
            Self::ElectronicByAgreement => {
                "by electronic transmission, by agreement of the parties"
            }
        }
    }
}

/// The names every method may be spelled, for an error message.
#[must_use]
pub fn method_names() -> String {
    ServiceMethod::ALL
        .iter()
        .map(|method| method.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One Service as the committed document pointer carries it under
/// `service:`, and as the certificate is composed from it. Names only: the
/// pointer is source-only, so it never carries a Person, Entity, Letter, or
/// email id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRecord {
    /// The recipient as the certificate names them.
    pub recipient: String,
    /// The party the recipient appears for, when counsel of record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counsel_for: Option<String>,
    pub method: ServiceMethod,
    /// `YYYY-MM-DD`.
    pub served_on: NaiveDate,
    /// The address or e-service handle the document went to.
    pub address: String,
    /// Who served it, as the declarant block names them.
    pub declarant: String,
}

/// One stored Service row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectService {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The served document revision (an Asset id).
    pub document_id: Uuid,
    pub recipient: String,
    pub recipient_person_id: Option<Uuid>,
    pub recipient_entity_id: Option<Uuid>,
    pub counsel_for: Option<String>,
    pub method: String,
    /// `YYYY-MM-DD`.
    pub served_on: String,
    pub address: String,
    pub declarant: String,
    pub declarant_person_id: Option<Uuid>,
    /// The outgoing Letter that is the evidence of a send Navigator made.
    pub letter_id: Option<Uuid>,
    /// The sent email that is the evidence of a send Navigator made.
    pub sent_email_id: Option<Uuid>,
    pub inserted_at: String,
    pub updated_at: String,
}

impl ProjectService {
    /// The committed shape of this row.
    ///
    /// # Panics
    /// Never: a stored row passed [`normalize`] before it was written.
    #[must_use]
    pub fn record(&self) -> ServiceRecord {
        ServiceRecord {
            recipient: self.recipient.clone(),
            counsel_for: self.counsel_for.clone(),
            method: ServiceMethod::parse(&self.method).unwrap_or(ServiceMethod::Mail),
            served_on: NaiveDate::parse_from_str(&self.served_on, "%Y-%m-%d").unwrap_or_default(),
            address: self.address.clone(),
            declarant: self.declarant.clone(),
        }
    }
}
