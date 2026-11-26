//! Firm, Project, and provider-account scoped authorization for email summaries.
//!
//! Approval and revocation events are append-only. The current approval row
//! is updated in place, while the audit table preserves who changed it.

use chrono::{DateTime, Utc};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::{
    firm_capability::{self, FirmCapability, FirmCapabilityDecision},
    persons::{self, Role},
    surreal::{record_id, record_uuid, SurrealDb},
};

pub const TABLE: &str = "summary_sender_approval";
const AUDIT_TABLE: &str = "summary_sender_approval_event";

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct SummarySenderApproval {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Option<Uuid>,
    pub provider_account: String,
    pub person_id: Uuid,
    pub sender_email: String,
    pub status: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("not authorized")]
    NotAuthorized,
    #[error("sender email must belong to an email-confirmed Person")]
    SenderNotVerified,
    #[error("sender must be the current Client DRI of this Project")]
    SenderNotClientDri,
    #[error("provider account must be a non-empty token")]
    InvalidProviderAccount,
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("person lookup: {0}")]
    Person(#[from] persons::PersonError),
    #[error("firm capability: {0}")]
    Firm(#[from] crate::firms::FirmError),
    #[error("Project: {0}")]
    Project(#[from] crate::projects::ProjectStoreError),
}

#[derive(SurrealValue)]
struct ApprovalRow {
    id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    project_id: Option<surrealdb::types::RecordId>,
    provider_account: String,
    person_id: surrealdb::types::RecordId,
    sender_email: String,
    status: String,
    updated_at: surrealdb::types::Datetime,
}

impl ApprovalRow {
    fn into_approval(self) -> Option<SummarySenderApproval> {
        Some(SummarySenderApproval {
            id: record_uuid(&self.id)?,
            firm_id: record_uuid(&self.firm_id)?,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            provider_account: self.provider_account,
            person_id: record_uuid(&self.person_id)?,
            sender_email: self.sender_email,
            status: self.status,
            updated_at: self.updated_at.into(),
        })
    }
}

const SELECT: &str =
    "id, firm_id, project_id, provider_account, person_id, sender_email, status, updated_at";

pub async fn is_approved(
    db: &SurrealDb,
    firm_id: Uuid,
    project_id: Uuid,
    provider_account: &str,
    person_id: Uuid,
    email: &str,
) -> Result<bool, ApprovalError> {
    let Some(project) = crate::projects::find_by_id(db, project_id)
        .await?
        .filter(|project| project.firm_id == Some(firm_id))
    else {
        return Ok(false);
    };
    drop(project);
    if !is_current_client_dri(db, project_id, person_id).await? {
        return Ok(false);
    }
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        let found: Option<serde_json::Value> = crate::postgres::matter::find(&pg, TABLE,
            serde_json::json!({"firm_id": firm_id,"project_id": project_id,"provider_account": provider_account,
                "person_id": person_id,"sender_email": email.trim().to_ascii_lowercase(),"status": "approved"})).await?;
        return Ok(found.is_some());
    }
    let mut response = db.query(format!("SELECT VALUE id FROM {TABLE} WHERE firm_id = $firm AND project_id = $project AND provider_account = $account AND person_id = $person AND sender_email = $email AND status = 'approved' LIMIT 1"))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .bind(("project", record_id(crate::projects::PROJECT_TABLE, project_id)))
        .bind(("account", provider_account.to_string()))
        .bind(("person", record_id("person", person_id)))
        .bind(("email", email.trim().to_ascii_lowercase()))
        .await.and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(!rows.is_empty())
}

pub async fn list_for_firm(
    db: &SurrealDb,
    role: Role,
    actor: Option<Uuid>,
    firm_id: Uuid,
) -> Result<Vec<SummarySenderApproval>, ApprovalError> {
    authorize(db, role, actor, firm_id).await?;
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        return Ok(crate::postgres::matter::select(
            &pg,
            TABLE,
            serde_json::json!({"firm_id": firm_id}),
            &["updated_at DESC", "id DESC"],
        )
        .await?);
    }

    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE firm_id = $firm ORDER BY updated_at DESC"
        ))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<ApprovalRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ApprovalRow::into_approval)
        .collect())
}

pub async fn audit_history_for_firm(
    db: &SurrealDb,
    role: Role,
    actor: Option<Uuid>,
    firm_id: Uuid,
) -> Result<Vec<ApprovalEvent>, ApprovalError> {
    authorize(db, role, actor, firm_id).await?;
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        return Ok(crate::postgres::matter::select(
            &pg,
            AUDIT_TABLE,
            serde_json::json!({"firm_id": firm_id}),
            &["inserted_at DESC", "id DESC"],
        )
        .await?);
    }

    let mut response = db.query(format!("SELECT id, firm_id, actor_person_id, project_id, provider_account, sender_email, person_id, action, inserted_at FROM {AUDIT_TABLE} WHERE firm_id = $firm ORDER BY inserted_at DESC"))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .await.and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<ApprovalEventRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ApprovalEventRow::into_event)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ApprovalEvent {
    pub id: Uuid,
    pub actor_person_id: Uuid,
    pub project_id: Option<Uuid>,
    pub sender_email: String,
    pub provider_account: String,
    pub action: String,
    pub inserted_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct ApprovalEventRow {
    id: surrealdb::types::RecordId,
    actor_person_id: surrealdb::types::RecordId,
    project_id: Option<surrealdb::types::RecordId>,
    provider_account: String,
    sender_email: String,
    action: String,
    inserted_at: surrealdb::types::Datetime,
}

impl ApprovalEventRow {
    fn into_event(self) -> Option<ApprovalEvent> {
        Some(ApprovalEvent {
            id: record_uuid(&self.id)?,
            actor_person_id: record_uuid(&self.actor_person_id)?,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            sender_email: self.sender_email,
            provider_account: self.provider_account,
            action: self.action,
            inserted_at: self.inserted_at.into(),
        })
    }
}

pub async fn approve(
    db: &SurrealDb,
    role: Role,
    actor: Uuid,
    firm_id: Uuid,
    project_id: Uuid,
    provider_account: &str,
    email: &str,
) -> Result<SummarySenderApproval, ApprovalError> {
    authorize(db, role, Some(actor), firm_id).await?;
    validate_account(provider_account)?;
    let email = email.trim().to_ascii_lowercase();
    let person = persons::find_by_email_ci(db, &email)
        .await?
        .filter(|person| person.email_confirmed)
        .ok_or(ApprovalError::SenderNotVerified)?;
    validate_project_sender(db, firm_id, project_id, person.id).await?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await?
        .ok_or(crate::postgres::matter::MatterAuthorityError::Unavailable)?;
    crate::postgres::summary_sender_approvals::approve(
        &pg,
        actor,
        firm_id,
        project_id,
        provider_account,
        &email,
        person.id,
    )
    .await
}

pub async fn revoke(
    db: &SurrealDb,
    role: Role,
    actor: Uuid,
    firm_id: Uuid,
    approval_id: Uuid,
) -> Result<(), ApprovalError> {
    authorize(db, role, Some(actor), firm_id).await?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await?
        .ok_or(crate::postgres::matter::MatterAuthorityError::Unavailable)?;
    crate::postgres::summary_sender_approvals::revoke(&pg, actor, firm_id, approval_id).await
}

async fn authorize(
    db: &SurrealDb,
    role: Role,
    actor: Option<Uuid>,
    firm_id: Uuid,
) -> Result<(), ApprovalError> {
    match firm_capability::resolve(
        db,
        role,
        actor,
        firm_id,
        FirmCapability::ManageSummaryApprovals,
    )
    .await?
    {
        FirmCapabilityDecision::Allowed => Ok(()),
        FirmCapabilityDecision::Forbidden | FirmCapabilityDecision::FirmNotFound => {
            Err(ApprovalError::NotAuthorized)
        }
    }
}

fn validate_account(account: &str) -> Result<(), ApprovalError> {
    if account.trim().is_empty() || account.chars().any(char::is_whitespace) {
        Err(ApprovalError::InvalidProviderAccount)
    } else {
        Ok(())
    }
}

async fn is_current_client_dri(
    db: &SurrealDb,
    project_id: Uuid,
    person_id: Uuid,
) -> Result<bool, ApprovalError> {
    let Some(person) = persons::find_by_id(db, person_id).await? else {
        return Ok(false);
    };
    if person.role != Role::Client {
        return Ok(false);
    }
    Ok(crate::projects::participations_for_project(db, project_id)
        .await?
        .into_iter()
        .any(|participation| {
            participation.person_id == person_id
                && participation.participation == "client"
                && participation.is_client_dri
        }))
}

async fn validate_project_sender(
    db: &SurrealDb,
    firm_id: Uuid,
    project_id: Uuid,
    person_id: Uuid,
) -> Result<(), ApprovalError> {
    let Some(project) = crate::projects::find_by_id(db, project_id).await? else {
        return Err(ApprovalError::NotAuthorized);
    };
    if project.firm_id != Some(firm_id) {
        return Err(ApprovalError::NotAuthorized);
    }
    if !is_current_client_dri(db, project_id, person_id).await? {
        return Err(ApprovalError::SenderNotClientDri);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        firms::{FirmMembership, NewFirm, NewPersonFirmRole},
        persons::{set_email_confirmed, NewPerson},
        test_support,
    };

    async fn setup() -> (SurrealDb, Uuid, Uuid, Uuid, Uuid, Uuid) {
        let db = SurrealDb::uninitialized();
        test_support::seed_compiled_brands_postgres(&db).await;
        let entity = crate::test_support::seed_entity(&db).await;
        let dri = persons::create(
            &db,
            &NewPerson::with_role("Firm DRI", "dri@example.com", Role::Admin),
        )
        .await
        .unwrap();
        let firm = crate::firms::create(
            &db,
            &NewFirm {
                name: "Firm A".into(),
                status: "active".into(),
                entity_id: entity,
                admin_dri_person_id: dri.id,
            },
        )
        .await
        .unwrap();
        let sender = persons::create(&db, &NewPerson::new("Sender", "sender@example.com"))
            .await
            .unwrap();
        set_email_confirmed(&db, sender.id, true).await.unwrap();
        let project = crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: format!("summary-{}", Uuid::now_v7()),
                name: "Summary matter".into(),
                status: "open".into(),
                entity_id: entity,
                firm_id: Some(firm.id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        crate::projects::designate_dri_in_surreal(
            &db,
            project.id,
            sender.id,
            crate::projects::DriSide::Client,
        )
        .await
        .unwrap();
        (db, firm.id, dri.id, sender.id, entity, project.id)
    }

    #[tokio::test]
    async fn approval_is_verified_scoped_revocable_and_audited() {
        test_support::with_required_postgres(async {
            let (db, firm, actor, _, _, project) = setup().await;
            let approval = approve(
                &db,
                Role::Admin,
                actor,
                firm,
                project,
                "sendgrid-account",
                "SENDER@example.com",
            )
            .await
            .unwrap();
            assert!(is_approved(
                &db,
                firm,
                project,
                "sendgrid-account",
                approval.person_id,
                "sender@example.com"
            )
            .await
            .unwrap());
            assert!(!is_approved(
                &db,
                firm,
                project,
                "other-account",
                approval.person_id,
                "sender@example.com"
            )
            .await
            .unwrap());
            assert!(!is_approved(
                &db,
                Uuid::now_v7(),
                project,
                "sendgrid-account",
                approval.person_id,
                "sender@example.com"
            )
            .await
            .unwrap());
            revoke(&db, Role::Admin, actor, firm, approval.id)
                .await
                .unwrap();
            assert!(!is_approved(
                &db,
                firm,
                project,
                "sendgrid-account",
                approval.person_id,
                "sender@example.com"
            )
            .await
            .unwrap());
            let rows = audit_history_for_firm(&db, Role::Admin, Some(actor), firm)
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn unverified_sender_and_non_dri_are_refused() {
        test_support::with_required_postgres(async {
            let (db, firm, actor, _, entity, project) = setup().await;
            let unverified =
                persons::create(&db, &NewPerson::new("Unverified", "unverified@example.com"))
                    .await
                    .unwrap();
            assert!(matches!(
                approve(
                    &db,
                    Role::Admin,
                    actor,
                    firm,
                    project,
                    "sendgrid-account",
                    &unverified.email
                )
                .await,
                Err(ApprovalError::SenderNotVerified)
            ));
            let other_admin = persons::create(
                &db,
                &NewPerson::with_role("Other admin", "other-admin@example.com", Role::Admin),
            )
            .await
            .unwrap();
            crate::firms::add_membership(
                &db,
                &NewPersonFirmRole {
                    person_id: other_admin.id,
                    firm_id: firm,
                    membership: FirmMembership::Admin,
                    is_dri: false,
                },
            )
            .await
            .unwrap();
            let verified =
                persons::create(&db, &NewPerson::new("Verified", "verified@example.com"))
                    .await
                    .unwrap();
            set_email_confirmed(&db, verified.id, true).await.unwrap();
            assert!(matches!(
                approve(
                    &db,
                    Role::Admin,
                    other_admin.id,
                    firm,
                    project,
                    "sendgrid-account",
                    &verified.email
                )
                .await,
                Err(ApprovalError::NotAuthorized)
            ));
            let _ = entity;
        })
        .await;
    }

    #[tokio::test]
    async fn an_email_reassigned_to_a_different_person_does_not_inherit_approval() {
        test_support::with_required_postgres(async {
            let (db, firm, actor, sender_id, _, project) = setup().await;
            let first = approve(
                &db,
                Role::Admin,
                actor,
                firm,
                project,
                "sendgrid-account",
                "sender@example.com",
            )
            .await
            .unwrap();
            persons::set_email_confirmed(&db, sender_id, false)
                .await
                .unwrap();
            // The data model keeps email unique. This simulates a later identity
            // taking the address after the original Person row has changed.
            let pg = crate::postgres::matter::authority_for(TABLE)
                .await
                .unwrap()
                .unwrap();
            sqlx::query("UPDATE person SET email = 'former@example.com' WHERE id = $1")
                .bind(sender_id)
                .execute(pg.pool())
                .await
                .unwrap();
            sqlx::query("DELETE FROM person_mailbox WHERE person_id = $1")
                .bind(sender_id)
                .execute(pg.pool())
                .await
                .unwrap();
            let replacement =
                persons::create(&db, &NewPerson::new("Replacement", "sender@example.com"))
                    .await
                    .unwrap();
            persons::set_email_confirmed(&db, replacement.id, true)
                .await
                .unwrap();
            // The old address remains linked to the former person. It cannot be
            // rebound to a new Person without retiring the old approval first.
            assert!(matches!(
                approve(
                    &db,
                    Role::Admin,
                    actor,
                    firm,
                    project,
                    "sendgrid-account",
                    "sender@example.com"
                )
                .await,
                Err(ApprovalError::SenderNotVerified | ApprovalError::SenderNotClientDri)
            ));
            assert!(is_approved(
                &db,
                firm,
                project,
                "sendgrid-account",
                first.person_id,
                "sender@example.com"
            )
            .await
            .unwrap());
            assert!(!is_approved(
                &db,
                firm,
                project,
                "sendgrid-account",
                replacement.id,
                "sender@example.com"
            )
            .await
            .unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn approvals_are_project_scoped_and_require_the_current_client_dri() {
        test_support::with_required_postgres(async {
            let (db, firm, actor, sender_id, entity, project_id) = setup().await;
            let other_project = crate::projects::create(
                &db,
                &crate::projects::NewProject {
                    code: format!("other-{}", Uuid::now_v7()),
                    name: "Other matter".into(),
                    status: "open".into(),
                    entity_id: entity,
                    firm_id: Some(firm),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let approval = approve(
                &db,
                Role::Admin,
                actor,
                firm,
                project_id,
                "sendgrid-account",
                "sender@example.com",
            )
            .await
            .unwrap();
            assert_eq!(approval.project_id, Some(project_id));
            assert!(!is_approved(
                &db,
                firm,
                other_project.id,
                "sendgrid-account",
                sender_id,
                "sender@example.com",
            )
            .await
            .unwrap());
            let participation =
                crate::projects::participation_for_person(&db, sender_id, project_id)
                    .await
                    .unwrap()
                    .unwrap();
            crate::projects::clear_dri_in_surreal(
                &db,
                participation.id,
                crate::projects::DriSide::Client,
            )
            .await
            .unwrap();
            assert!(!is_approved(
                &db,
                firm,
                project_id,
                "sendgrid-account",
                sender_id,
                "sender@example.com",
            )
            .await
            .unwrap());
        })
        .await;
    }
}
