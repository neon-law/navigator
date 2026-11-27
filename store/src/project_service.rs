//! Project-scoped [Service](../../docs/glossary/service.md) rows
//! (`project_service`): one served document × one recipient × one method ×
//! one date.
//!
//! A write stores the date the caller named and the method as the rules
//! spell it. Nothing here computes a response deadline from a service
//! record; the Deadlines module owns what is due.

use chrono::NaiveDate;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
pub use model::project_service::{method_names, ProjectService, ServiceMethod, ServiceRecord};

const TABLE: &str = "project_service";

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("{0}")]
    Invalid(String),
    #[error("no such service record")]
    NotFound,
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<(), ServiceError> {
    let asset = crate::assets::find_by_id(application_store, document_id)
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
pub async fn create(
    application_store: &ApplicationStore,
    new: &NewService,
) -> Result<ProjectService, ServiceError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    let normalized = normalize(new)?;
    require_project_document(application_store, new.project_id, new.document_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    Ok(crate::postgres::matter::upsert(&pg, TABLE, &serde_json::json!({
        "id": Uuid::now_v7(), "project_id": new.project_id, "document_id": new.document_id,
        "recipient": normalized.recipient, "recipient_person_id": new.recipient_person_id,
        "recipient_entity_id": new.recipient_entity_id, "counsel_for": normalized.counsel_for,
        "method": normalized.method, "served_on": normalized.served_on, "address": normalized.address,
        "declarant": normalized.declarant, "declarant_person_id": new.declarant_person_id,
        "letter_id": new.letter_id, "sent_email_id": new.sent_email_id, "inserted_at": now, "updated_at": now,
    })).await?)
}

/// Every Service on a Project, earliest served first.
///
/// # Errors
/// Propagates any database error.
pub async fn list(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<ProjectService>, ServiceError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::select(
        &pg,
        TABLE,
        serde_json::json!({"project_id": project_id}),
        &["served_on", "id"],
    )
    .await?)
}

/// One Service on a Project, or `None`.
///
/// # Errors
/// Propagates any database error.
pub async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<ProjectService>, ServiceError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::find(
        &pg,
        TABLE,
        serde_json::json!({"id": id, "project_id": project_id}),
    )
    .await?)
}

/// Remove every Service on a Project — the matter-delete cascade.
///
/// # Errors
/// Propagates any database error.
pub async fn delete_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), ServiceError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    crate::postgres::matter::delete(&pg, TABLE, serde_json::json!({"project_id": project_id}))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{create, delete_for_project, find, list, NewService, ServiceMethod};
    use uuid::Uuid;

    async fn filed_document(
        application_store: &crate::application_store::ApplicationStore,
        project_id: Uuid,
    ) -> (Uuid, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
            cloud::FsStorage::new(tmp.path().to_path_buf())
                .await
                .unwrap(),
        );
        let filed = crate::documents::ingest_bytes_as(
            application_store,
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
        crate::test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            crate::test_support::seed_compiled_brands_postgres(&application_store).await;
            let project_id =
                crate::test_support::seed_project_id(&application_store, "service").await;
            let (document_id, _tmp) = filed_document(&application_store, project_id).await;
            let mut second = sample(project_id, document_id);
            second.recipient = "Other Counsel".into();
            second.method = "e_service".into();
            second.served_on = "2026-03-04".into();
            second.address = "other@example.com".into();
            let later = create(&application_store, &sample(project_id, document_id))
                .await
                .unwrap();
            let earlier = create(&application_store, &second).await.unwrap();
            assert_eq!(later.method, "mail");
            assert_eq!(later.counsel_for.as_deref(), Some("Defendant Example Co."));
            let rows = list(&application_store, project_id).await.unwrap();
            let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
            assert_eq!(ids, [earlier.id, later.id], "earliest served first");
            assert_eq!(
                find(&application_store, project_id, later.id)
                    .await
                    .unwrap(),
                Some(later.clone())
            );
            assert_eq!(
                find(&application_store, Uuid::now_v7(), later.id)
                    .await
                    .unwrap(),
                None
            );
            let record = later.record();
            assert_eq!(record.method, ServiceMethod::Mail);
            assert_eq!(record.served_on.to_string(), "2026-03-05");
            delete_for_project(&application_store, Uuid::now_v7())
                .await
                .unwrap();
            assert_eq!(list(&application_store, project_id).await.unwrap().len(), 2);
            delete_for_project(&application_store, project_id)
                .await
                .unwrap();
            assert!(list(&application_store, project_id)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn an_unknown_method_a_non_date_and_two_recipients_are_refused() {
        crate::test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            crate::test_support::seed_compiled_brands_postgres(&application_store).await;
            let project_id =
                crate::test_support::seed_project_id(&application_store, "service").await;
            let (document_id, _tmp) = filed_document(&application_store, project_id).await;
            let mut unknown = sample(project_id, document_id);
            unknown.method = "carrier_pigeon".into();
            let error = create(&application_store, &unknown).await.unwrap_err();
            assert!(error.to_string().contains("certified_mail"), "{error}");
            let mut non_date = sample(project_id, document_id);
            non_date.served_on = "March 5, 2026".into();
            assert!(create(&application_store, &non_date).await.is_err());
            let mut both = sample(project_id, document_id);
            both.recipient_person_id = Some(Uuid::now_v7());
            both.recipient_entity_id = Some(Uuid::now_v7());
            let error = create(&application_store, &both).await.unwrap_err();
            assert!(error.to_string().contains("not both"), "{error}");
            let mut blank = sample(project_id, document_id);
            blank.declarant = "  ".into();
            assert!(create(&application_store, &blank).await.is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn a_document_from_another_project_is_refused() {
        crate::test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            crate::test_support::seed_compiled_brands_postgres(&application_store).await;
            let project_id =
                crate::test_support::seed_project_id(&application_store, "service").await;
            let other = crate::test_support::seed_project_id(&application_store, "other").await;
            let (document_id, _tmp) = filed_document(&application_store, other).await;
            let error = create(&application_store, &sample(project_id, document_id))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("this Project"), "{error}");
            let mut nowhere = sample(project_id, document_id);
            nowhere.document_id = Uuid::now_v7();
            assert!(create(&application_store, &nowhere).await.is_err());
        })
        .await;
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
