//! Firm, Project, and provider-account scoped authorization for email summaries.
//!
//! Approval and revocation events are append-only. The current approval row
//! is updated in place, while the audit table preserves who changed it.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    application_store::ApplicationStore,
    firm_capability::{self, FirmCapability, FirmCapabilityDecision},
    persons::{self, Role},
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
    #[error("person lookup: {0}")]
    Person(#[from] persons::PersonError),
    #[error("firm capability: {0}")]
    Firm(#[from] crate::firms::FirmError),
    #[error("Project: {0}")]
    Project(#[from] crate::projects::ProjectStoreError),
}

pub async fn is_approved(
    db: &ApplicationStore,
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
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    let found: Option<serde_json::Value> = crate::postgres::matter::find(&pg, TABLE,
        serde_json::json!({"firm_id": firm_id,"project_id": project_id,"provider_account": provider_account,
            "person_id": person_id,"sender_email": email.trim().to_ascii_lowercase(),"status": "approved"})).await?;
    Ok(found.is_some())
}

pub async fn list_for_firm(
    db: &ApplicationStore,
    role: Role,
    actor: Option<Uuid>,
    firm_id: Uuid,
) -> Result<Vec<SummarySenderApproval>, ApprovalError> {
    authorize(db, role, actor, firm_id).await?;
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::select(
        &pg,
        TABLE,
        serde_json::json!({"firm_id": firm_id}),
        &["updated_at DESC", "id DESC"],
    )
    .await?)
}

pub async fn audit_history_for_firm(
    db: &ApplicationStore,
    role: Role,
    actor: Option<Uuid>,
    firm_id: Uuid,
) -> Result<Vec<ApprovalEvent>, ApprovalError> {
    authorize(db, role, actor, firm_id).await?;
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::select(
        &pg,
        AUDIT_TABLE,
        serde_json::json!({"firm_id": firm_id}),
        &["inserted_at DESC", "id DESC"],
    )
    .await?)
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

pub async fn approve(
    db: &ApplicationStore,
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
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
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
    db: &ApplicationStore,
    role: Role,
    actor: Uuid,
    firm_id: Uuid,
    approval_id: Uuid,
) -> Result<(), ApprovalError> {
    authorize(db, role, Some(actor), firm_id).await?;
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    crate::postgres::summary_sender_approvals::revoke(&pg, actor, firm_id, approval_id).await
}

async fn authorize(
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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

    async fn setup() -> (ApplicationStore, Uuid, Uuid, Uuid, Uuid, Uuid) {
        let db = ApplicationStore::uninitialized();
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
        crate::projects::designate_dri(
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
            let pg = crate::postgres::matter::authority_for(TABLE).await.unwrap();
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
            crate::projects::clear_dri(&db, participation.id, crate::projects::DriSide::Client)
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
