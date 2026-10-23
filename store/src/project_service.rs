//! Project-scoped [Service](../../docs/glossary/service.md) rows
//! (`project_service`): one served document × one recipient × one method ×
//! one date.
//!
//! A write stores the date the caller named and the method as the rules
//! spell it. Nothing here computes a response deadline from a service
//! record; the Deadlines module owns what is due.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "project_service";

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

#[derive(SurrealValue)]
struct Row {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    document_id: surrealdb::types::RecordId,
    recipient: String,
    recipient_person_id: Option<surrealdb::types::RecordId>,
    recipient_entity_id: Option<surrealdb::types::RecordId>,
    counsel_for: Option<String>,
    method: String,
    served_on: String,
    address: String,
    declarant: String,
    declarant_person_id: Option<surrealdb::types::RecordId>,
    letter_id: Option<surrealdb::types::RecordId>,
    sent_email_id: Option<surrealdb::types::RecordId>,
    inserted_at: String,
    updated_at: String,
}

impl Row {
    fn into_service(self) -> Option<ProjectService> {
        Some(ProjectService {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            document_id: record_uuid(&self.document_id)?,
            recipient: self.recipient,
            recipient_person_id: self.recipient_person_id.as_ref().and_then(record_uuid),
            recipient_entity_id: self.recipient_entity_id.as_ref().and_then(record_uuid),
            counsel_for: self.counsel_for,
            method: self.method,
            served_on: self.served_on,
            address: self.address,
            declarant: self.declarant,
            declarant_person_id: self.declarant_person_id.as_ref().and_then(record_uuid),
            letter_id: self.letter_id.as_ref().and_then(record_uuid),
            sent_email_id: self.sent_email_id.as_ref().and_then(record_uuid),
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "id, project_id, document_id, recipient, recipient_person_id, \
     recipient_entity_id, counsel_for, method, served_on, address, declarant, \
     declarant_person_id, letter_id, sent_email_id, inserted_at, updated_at";

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no such service record")]
    NotFound,
    #[error("writing a service record returned no usable row")]
    WriteReturnedNothing,
}

/// What a new Service needs.
#[derive(Debug, Clone, Default)]
pub struct NewService {
    pub project_id: Uuid,
    pub document_id: Uuid,
    pub recipient: String,
    pub recipient_person_id: Option<Uuid>,
    pub recipient_entity_id: Option<Uuid>,
    pub counsel_for: Option<String>,
    pub method: String,
    pub served_on: String,
    pub address: String,
    pub declarant: String,
    pub declarant_person_id: Option<Uuid>,
    pub letter_id: Option<Uuid>,
    pub sent_email_id: Option<Uuid>,
}

struct Normalized {
    recipient: String,
    counsel_for: Option<String>,
    method: &'static str,
    served_on: String,
    address: String,
    declarant: String,
}

fn non_blank(value: &str, field: &str) -> Result<String, ServiceError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ServiceError::Invalid(format!("{field} must not be blank")))
    } else {
        Ok(value.to_string())
    }
}

/// Enforce what the field types cannot: a known method, a real calendar
/// date, non-blank names, and one recipient link at most.
fn normalize(new: &NewService) -> Result<Normalized, ServiceError> {
    let method = ServiceMethod::parse(&new.method).ok_or_else(|| {
        ServiceError::Invalid(format!("method must be one of {}", method_names()))
    })?;
    let served_on = NaiveDate::parse_from_str(new.served_on.trim(), "%Y-%m-%d")
        .map_err(|_| ServiceError::Invalid("served_on must be a date, YYYY-MM-DD".into()))?;
    if new.recipient_person_id.is_some() && new.recipient_entity_id.is_some() {
        return Err(ServiceError::Invalid(
            "a recipient is a Person or an Entity, not both".into(),
        ));
    }
    Ok(Normalized {
        recipient: non_blank(&new.recipient, "recipient")?,
        counsel_for: new
            .counsel_for
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        method: method.as_str(),
        served_on: served_on.format("%Y-%m-%d").to_string(),
        address: non_blank(&new.address, "address")?,
        declarant: non_blank(&new.declarant, "declarant")?,
    })
}

/// The served document must be a document filed on this Project.
async fn require_project_document(
    surreal: &SurrealDb,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<(), ServiceError> {
    let asset = crate::assets::find_by_id(surreal, document_id)
        .await
        .map_err(|error| ServiceError::Invalid(error.to_string()))?;
    match asset {
        Some(asset) if asset.project_id == Some(project_id) => Ok(()),
        _ => Err(ServiceError::Invalid(
            "document_id names no document on this Project".into(),
        )),
    }
}

/// Record one Service and return the stored row.
///
/// # Errors
/// [`ServiceError::Invalid`] for a bad method, date, blank name, two
/// recipient links, or a document from another Project; any database error.
pub async fn create(surreal: &SurrealDb, new: &NewService) -> Result<ProjectService, ServiceError> {
    let normalized = normalize(new)?;
    require_project_document(surreal, new.project_id, new.document_id).await?;
    let id = Uuid::now_v7();
    let now = chrono::Utc::now().to_rfc3339();
    let mut response = surreal
        .query(format!(
            "CREATE $id SET \
             project_id = $project_id, document_id = $document_id, recipient = $recipient, \
             recipient_person_id = $recipient_person_id, \
             recipient_entity_id = $recipient_entity_id, counsel_for = $counsel_for, \
             method = $method, served_on = $served_on, address = $address, \
             declarant = $declarant, declarant_person_id = $declarant_person_id, \
             letter_id = $letter_id, sent_email_id = $sent_email_id, \
             inserted_at = $now, updated_at = $now \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, new.project_id),
        ))
        .bind((
            "document_id",
            record_id(crate::assets::TABLE, new.document_id),
        ))
        .bind(("recipient", normalized.recipient))
        .bind((
            "recipient_person_id",
            new.recipient_person_id.map(|p| record_id("person", p)),
        ))
        .bind((
            "recipient_entity_id",
            new.recipient_entity_id.map(|e| record_id("entity", e)),
        ))
        .bind(("counsel_for", normalized.counsel_for))
        .bind(("method", normalized.method.to_string()))
        .bind(("served_on", normalized.served_on))
        .bind(("address", normalized.address))
        .bind(("declarant", normalized.declarant))
        .bind((
            "declarant_person_id",
            new.declarant_person_id.map(|p| record_id("person", p)),
        ))
        .bind(("letter_id", new.letter_id.map(|l| record_id("letter", l))))
        .bind((
            "sent_email_id",
            new.sent_email_id.map(|e| record_id("sent_email", e)),
        ))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<Row> = response.take(0)?;
    let service = row
        .and_then(Row::into_service)
        .ok_or(ServiceError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::ProjectService(service.id),
    )
    .await;
    Ok(service)
}

/// Every Service on a Project, earliest served first.
///
/// # Errors
/// Propagates any database error.
pub async fn list(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<ProjectService>, ServiceError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project \
             ORDER BY served_on ASC, id ASC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<Row> = response.take(0)?;
    Ok(rows.into_iter().filter_map(Row::into_service).collect())
}

/// One Service on a Project, or `None`.
///
/// # Errors
/// Propagates any database error.
pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<ProjectService>, ServiceError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE id = $id AND project_id = $project LIMIT 1"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<Row> = response.take(0)?;
    Ok(rows.into_iter().find_map(Row::into_service))
}

/// Remove every Service on a Project — the matter-delete cascade.
///
/// # Errors
/// Propagates any database error.
pub async fn delete_for_project(surreal: &SurrealDb, project_id: Uuid) -> Result<(), ServiceError> {
    let mut response = surreal
        .query(format!(
            "DELETE {TABLE} WHERE project_id = $project RETURN BEFORE"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let deleted: Vec<Row> = response.take(0)?;
    crate::postgres_mirror::after_writes(
        surreal,
        deleted
            .into_iter()
            .filter_map(Row::into_service)
            .map(|service| crate::postgres_mirror::MirrorKey::ProjectService(service.id)),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{create, find, list, NewService, ServiceMethod};
    use uuid::Uuid;

    async fn filed_document(
        surreal: &crate::surreal::SurrealDb,
        project_id: Uuid,
    ) -> (Uuid, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
            cloud::FsStorage::new(tmp.path().to_path_buf())
                .await
                .unwrap(),
        );
        let filed = crate::documents::ingest_bytes_as(
            surreal,
            &storage,
            &crate::documents::IngestArgs {
                project_id,
                source: crate::documents::source::UPLOAD,
                filename: "motion.pdf",
                kind: "pleading",
                content_type: "application/pdf",
                description: None,
                visibility: crate::documents::visibility::INTERNAL,
                secondary_storage_key: None,
            },
            &crate::documents::DocumentIdentity {
                slug: Some("pleadings/motion.pdf"),
                ..Default::default()
            },
            b"%PDF-1.7 synthetic",
        )
        .await
        .unwrap();
        (filed.asset_id, tmp)
    }

    fn sample(project_id: Uuid, document_id: Uuid) -> NewService {
        NewService {
            project_id,
            document_id,
            recipient: "Example Counsel".into(),
            counsel_for: Some("Defendant Example Co.".into()),
            method: "mail".into(),
            served_on: "2026-03-05".into(),
            address: "200 Sample Street, Sampleton, CA 90000".into(),
            declarant: "Pat Sample".into(),
            ..NewService::default()
        }
    }

    #[tokio::test]
    async fn create_list_and_find_round_trip() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "service").await;
        let (document_id, _tmp) = filed_document(&surreal, project_id).await;
        let mut second = sample(project_id, document_id);
        second.recipient = "Other Counsel".into();
        second.method = "e_service".into();
        second.served_on = "2026-03-04".into();
        second.address = "other@example.com".into();
        let later = create(&surreal, &sample(project_id, document_id))
            .await
            .unwrap();
        let earlier = create(&surreal, &second).await.unwrap();
        assert_eq!(later.method, "mail");
        assert_eq!(later.counsel_for.as_deref(), Some("Defendant Example Co."));
        let rows = list(&surreal, project_id).await.unwrap();
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        assert_eq!(ids, [earlier.id, later.id], "earliest served first");
        assert_eq!(
            find(&surreal, project_id, later.id).await.unwrap(),
            Some(later.clone())
        );
        assert_eq!(
            find(&surreal, Uuid::now_v7(), later.id).await.unwrap(),
            None
        );
        let record = later.record();
        assert_eq!(record.method, ServiceMethod::Mail);
        assert_eq!(record.served_on.to_string(), "2026-03-05");
    }

    #[tokio::test]
    async fn an_unknown_method_a_non_date_and_two_recipients_are_refused() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "service").await;
        let (document_id, _tmp) = filed_document(&surreal, project_id).await;
        let mut unknown = sample(project_id, document_id);
        unknown.method = "carrier_pigeon".into();
        let error = create(&surreal, &unknown).await.unwrap_err();
        assert!(error.to_string().contains("certified_mail"), "{error}");
        let mut non_date = sample(project_id, document_id);
        non_date.served_on = "March 5, 2026".into();
        assert!(create(&surreal, &non_date).await.is_err());
        let mut both = sample(project_id, document_id);
        both.recipient_person_id = Some(Uuid::now_v7());
        both.recipient_entity_id = Some(Uuid::now_v7());
        let error = create(&surreal, &both).await.unwrap_err();
        assert!(error.to_string().contains("not both"), "{error}");
        let mut blank = sample(project_id, document_id);
        blank.declarant = "  ".into();
        assert!(create(&surreal, &blank).await.is_err());
    }

    #[tokio::test]
    async fn a_document_from_another_project_is_refused() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "service").await;
        let other = crate::test_support::seed_project_surreal(&surreal, "other").await;
        let (document_id, _tmp) = filed_document(&surreal, other).await;
        let error = create(&surreal, &sample(project_id, document_id))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("this Project"), "{error}");
        let mut nowhere = sample(project_id, document_id);
        nowhere.document_id = Uuid::now_v7();
        assert!(create(&surreal, &nowhere).await.is_err());
    }

    #[test]
    fn every_method_round_trips_and_has_a_certificate_phrase() {
        for method in ServiceMethod::ALL {
            assert_eq!(ServiceMethod::parse(method.as_str()), Some(method));
            assert!(method.certificate_phrase().starts_with("by "));
        }
        assert_eq!(ServiceMethod::parse("fax"), None);
    }
}
