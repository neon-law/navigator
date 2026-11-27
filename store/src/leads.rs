//! Public lead capture records.
//!
//! A lead is a request for contact, not an identity. Capture never creates a
//! `person` row. Conversion and linking write through [`crate::persons`] and
//! set `lead.person_id`, so the Person table stays the human directory.
//! Repeated requests for one mailbox on one brand update the same row and
//! increment `submissions`; the email index is intentionally non-unique
//! because it is a lookup aid, not an identity constraint.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::persons::{self, ContactUpdate, NewPerson, PersonError};

const TABLE: &str = "lead";
const CONSENT_TABLE: &str = "lead_consent";

/// Closed status set stored on `lead.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadStatus {
    New,
    Contacted,
    Converted,
    Declined,
    Unsubscribed,
}

impl LeadStatus {
    /// Stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Contacted => "contacted",
            Self::Converted => "converted",
            Self::Declined => "declined",
            Self::Unsubscribed => "unsubscribed",
        }
    }

    /// Parse a stored or submitted status word.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "new" => Some(Self::New),
            "contacted" => Some(Self::Contacted),
            "converted" => Some(Self::Converted),
            "declined" => Some(Self::Declined),
            "unsubscribed" => Some(Self::Unsubscribed),
            _ => None,
        }
    }
}

/// One captured public contact request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Lead {
    pub id: Uuid,
    pub email: String,
    pub email_lower: String,
    pub phone: Option<String>,
    pub brand_key: String,
    pub source_path: String,
    pub consent_version: String,
    pub consented_at: DateTime<Utc>,
    pub status: String,
    pub unsubscribed_at: Option<DateTime<Utc>>,
    pub person_id: Option<Uuid>,
    pub submissions: i64,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The values a public lead submission records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLead {
    pub email: String,
    pub phone: Option<String>,
    pub brand_key: String,
    pub source_path: String,
    pub consent_version: String,
    pub consented_at: DateTime<Utc>,
}

/// The immutable SMS and callback consent tuple captured with a public lead
/// submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewConsentEvent {
    pub consented_at: DateTime<Utc>,
    pub phone: Option<String>,
    pub source_path: String,
    pub sms_consent_version: String,
    pub sms_policy_version: String,
    pub sms_opt_in: bool,
    pub call_consent_version: String,
    pub call_opt_in: bool,
}

/// One immutable consent event, oldest first in [`consent_history`]. An event
/// captured before the callback choice existed carries no callback wording
/// and no consent to a call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ConsentEvent {
    pub id: Uuid,
    pub lead_id: Uuid,
    pub consented_at: DateTime<Utc>,
    pub phone: Option<String>,
    pub source_path: String,
    pub sms_consent_version: String,
    pub sms_policy_version: String,
    pub sms_opt_in: bool,
    pub call_consent_version: Option<String>,
    pub call_opt_in: bool,
}

/// Errors reading or writing public lead records.
#[derive(Debug, thiserror::Error)]
pub enum LeadError {
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("writing a lead returned no usable row")]
    WriteReturnedNothing,
    #[error("unknown lead")]
    NotFound,
    #[error("status is not one of new, contacted, converted, declined, unsubscribed")]
    InvalidStatus,
    #[error("converted status is set only when a Person is linked")]
    ConvertedWithoutPerson,
    #[error("no person holds this mailbox")]
    NoMatchingPerson,
    #[error(transparent)]
    Person(#[from] PersonError),
}

/// Last four digits of a phone, or an em dash when none were recorded.
#[must_use]
pub fn mask_phone(phone: Option<&str>) -> String {
    let digits: String = phone
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    match digits.len() {
        0 => "—".to_string(),
        n if n <= 4 => format!("…{digits}"),
        n => format!("…{}", &digits[n - 4..]),
    }
}

/// Insert a lead or update the existing `(email_lower, brand_key)` row.
///
/// The lookup and update are deliberately application-level rather than a
/// unique constraint: the schema permits several rows for the same mailbox so
/// the later admin queue can retain a brand-scoped request without making a
/// mailbox an identity key.
pub async fn record(_db: &ApplicationStore, new: &NewLead) -> Result<Lead, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    crate::postgres::leads::record(&pg, new, None, None).await
}

/// Answers submitted with a public contact request.
pub struct LeadIntake<'a> {
    pub submission_key: String,
    pub concern: &'a str,
    pub details: LeadIntakeDetails<'a>,
}

/// Additional answers collected by the contact page.
pub enum LeadIntakeDetails<'a> {
    Iep {
        district: &'a str,
        relationship: &'a str,
        next_date: Option<chrono::NaiveDate>,
    },
    Contact {
        name: &'a str,
    },
}

/// Save the submitted answers, contact details and consent together under the receiving Firm.
pub async fn record_intake(
    new: &NewLead,
    event: &NewConsentEvent,
    answers: &LeadIntake<'_>,
) -> Result<Lead, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    crate::postgres::leads::record(&pg, new, Some(event), Some(answers)).await
}

/// Read submissions in the Firm that received them, even if the Brand subsequently moves.
pub async fn intake_history(
    lead: Uuid,
    viewer: Uuid,
    role: persons::Role,
) -> Result<Vec<String>, LeadError> {
    if !matches!(role, persons::Role::Owner | persons::Role::Admin) {
        return Ok(Vec::new());
    }
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    sqlx::query_scalar("SELECT concat_ws(E'\\n', submitted_at::date::text,
        CASE WHEN contact_name IS NULL THEN concat_ws(E'\\n',
            'School or district: ' || school_district, 'Relationship: ' || relationship,
            'Concern: ' || concern, 'Upcoming date: ' || coalesce(next_date::text, 'None given'))
        ELSE concat_ws(E'\\n', 'Name: ' || contact_name, 'Inquiry: ' || concern) END)
        FROM lead_intake i WHERE lead_id=$1 AND ($2 OR EXISTS (
        SELECT 1 FROM person_firm_role m WHERE m.person_id=$3 AND m.firm_id=i.firm_id AND m.membership='admin'
    )) ORDER BY submitted_at DESC, i.id DESC")
        .bind(lead).bind(role == persons::Role::Owner).bind(viewer)
        .fetch_all(pg.pool()).await.map_err(|e| crate::postgres::matter::MatterAuthorityError::from(e).into())
}

/// Record one immutable consent event and update the lead's current contact
/// projection in one transaction.
///
/// SurrealDB has no transaction object in this store seam, so the explicit
/// query writes the event first and the lead second inside `BEGIN`/`COMMIT`.
/// The event carries the phone, page, exact wording, policy version, and opt-in
/// state that belong to the same submission; the lead row keeps only current
/// contact data.
pub async fn record_consent_event(
    _db: &ApplicationStore,
    new: &NewLead,
    event: &NewConsentEvent,
) -> Result<Lead, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    crate::postgres::leads::record(&pg, new, Some(event), None).await
}

/// Read every consent event ordered by PostgreSQL's microsecond precision and UUID ties.
pub async fn consent_history(
    _db: &ApplicationStore,
    lead_id: Uuid,
) -> Result<Vec<ConsentEvent>, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::select(
        &pg,
        CONSENT_TABLE,
        serde_json::json!({"lead_id": lead_id}),
        &["consented_at", "id"],
    )
    .await?)
}

/// Read the newest consent event for the admin lead surface.
pub async fn latest_consent_event(
    _db: &ApplicationStore,
    lead_id: Uuid,
) -> Result<Option<ConsentEvent>, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    let rows: Vec<ConsentEvent> = crate::postgres::matter::select(
        &pg,
        CONSENT_TABLE,
        serde_json::json!({"lead_id": lead_id}),
        &["consented_at DESC", "id DESC"],
    )
    .await?;
    Ok(rows.into_iter().next())
}

/// List leads newest first for the later admin queue.
pub async fn list(_db: &ApplicationStore) -> Result<Vec<Lead>, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::select(
        &pg,
        TABLE,
        serde_json::json!({}),
        &["updated_at DESC", "id DESC"],
    )
    .await?)
}

/// One lead by id.
pub async fn find(_db: &ApplicationStore, id: Uuid) -> Result<Option<Lead>, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::find(&pg, TABLE, serde_json::json!({"id": id})).await?)
}

/// Persist a status word. `unsubscribed_at` is written the first time the
/// row becomes unsubscribed and left alone afterward.
pub async fn set_status(
    db: &ApplicationStore,
    id: Uuid,
    status: LeadStatus,
) -> Result<Lead, LeadError> {
    let existing = find(db, id).await?.ok_or(LeadError::NotFound)?;
    if status == LeadStatus::Converted && existing.person_id.is_none() {
        return Err(LeadError::ConvertedWithoutPerson);
    }
    apply_status(db, existing, status).await
}

/// Create a Client Person from the lead mailbox and mark the row converted.
///
/// If an earlier attempt created the Person but did not link the lead, retrying
/// completes that link. This makes the two writes recoverable without risking
/// an unlinked Person that can sign in.
pub async fn convert(db: &ApplicationStore, id: Uuid) -> Result<Lead, LeadError> {
    let existing = find(db, id).await?.ok_or(LeadError::NotFound)?;
    if let Some(person) = persons::find_by_email_ci(db, &existing.email).await? {
        return link_converted(db, existing.id, person.id).await;
    }
    let mut input = NewPerson::new(existing.email.clone(), existing.email.clone());
    input.phone = existing.phone.clone();
    let person = persons::create(db, &input).await?;
    link_converted(db, existing.id, person.id).await
}

/// Point the lead at the Person who already holds its mailbox and mark it
/// converted.
///
/// When that Person has no phone and the lead recorded one, the phone is
/// copied onto the Person row so later contact reads the directory, not the
/// queue.
pub async fn link_person(db: &ApplicationStore, id: Uuid) -> Result<Lead, LeadError> {
    let existing = find(db, id).await?.ok_or(LeadError::NotFound)?;
    let Some(person) = persons::find_by_email_ci(db, &existing.email).await? else {
        return Err(LeadError::NoMatchingPerson);
    };
    if person.phone.is_none() {
        if let Some(phone) = existing.phone.clone() {
            persons::update_contact(
                db,
                person.id,
                &ContactUpdate {
                    name: person.name.clone(),
                    title: person.title.clone(),
                    phone: Some(phone),
                },
            )
            .await?;
        }
    }
    link_converted(db, existing.id, person.id).await
}

async fn apply_status(
    _db: &ApplicationStore,
    existing: Lead,
    status: LeadStatus,
) -> Result<Lead, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    let row: serde_json::Value = sqlx::query_scalar("UPDATE lead AS r SET status=$2, unsubscribed_at=CASE WHEN $2='unsubscribed' THEN COALESCE(unsubscribed_at,now()) ELSE unsubscribed_at END, updated_at=now() WHERE id=$1 RETURNING to_jsonb(r)")
        .bind(existing.id).bind(status.as_str()).fetch_optional(pg.pool()).await
        .map_err(crate::postgres::matter::MatterAuthorityError::from)?.ok_or(LeadError::WriteReturnedNothing)?;
    serde_json::from_value(row)
        .map_err(crate::postgres::matter::MatterAuthorityError::from)
        .map_err(LeadError::from)
}

async fn link_converted(
    _db: &ApplicationStore,
    lead_id: Uuid,
    person_id: Uuid,
) -> Result<Lead, LeadError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    crate::postgres::matter::update(&pg, TABLE, serde_json::json!({"id": lead_id}), serde_json::json!({"status": "converted", "person_id": person_id, "updated_at": Utc::now()})).await?.into_iter().next().ok_or(LeadError::WriteReturnedNothing)
}

#[cfg(test)]
mod tests {
    use chrono::{SubsecRound, Utc};

    use super::{
        consent_history, convert, find, latest_consent_event, link_person, list, mask_phone,
        record, record_consent_event, set_status, LeadError, LeadStatus, NewConsentEvent, NewLead,
    };
    use crate::persons::{self, NewPerson, Role};
    use uuid::Uuid;

    fn lead(email: &str, brand_key: &str) -> NewLead {
        NewLead {
            email: email.to_string(),
            phone: None,
            brand_key: brand_key.to_string(),
            source_path: "/contact".to_string(),
            consent_version: "By sending this, you agree.".to_string(),
            consented_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn iep_intake_retries_and_siblings_keep_firm_scoped_answers() {
        let (options, database) = crate::postgres::fixtures::test_postgres();
        let fixture = crate::test_support::PostgresFixture::create(options.database(&database))
            .await
            .unwrap();
        let pg = fixture.postgres();
        let admin: Uuid = sqlx::query_scalar("INSERT INTO person (name,email,role) VALUES ('Synthetic Admin','admin@example.test','admin') RETURNING id")
            .fetch_one(pg.pool()).await.unwrap();
        let firm: Uuid = sqlx::query_scalar("INSERT INTO firm (name,status,slug) VALUES ('Synthetic Firm','active','synthetic-iep') RETURNING id")
            .fetch_one(pg.pool()).await.unwrap();
        sqlx::query("INSERT INTO brand (name,brand_key,firm_id) VALUES ('Synthetic Brand','synthetic-iep',$1)")
            .bind(firm).execute(pg.pool()).await.unwrap();
        sqlx::query("INSERT INTO firm_brand (firm_id,brand_key) VALUES ($1,'synthetic-iep')")
            .bind(firm)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO person_firm_role (person_id,firm_id,membership) VALUES ($1,$2,'admin')",
        )
        .bind(admin)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
        crate::postgres::matter::with_test_db(pg.clone(), async {
            let lead = lead("family@example.test", "synthetic-iep");
            let event = NewConsentEvent {
                consented_at: lead.consented_at,
                phone: None,
                source_path: lead.source_path.clone(),
                sms_consent_version: "test".into(),
                sms_policy_version: "test".into(),
                sms_opt_in: false,
                call_consent_version: "test".into(),
                call_opt_in: false,
            };
            let mut answers = super::LeadIntake {
                submission_key: "first".into(),
                concern: "Evaluation",
                details: super::LeadIntakeDetails::Iep {
                    district: "Example District",
                    relationship: "Parent",
                    next_date: None,
                },
            };
            let first = super::record_intake(&lead, &event, &answers).await.unwrap();
            let retry = super::record_intake(&lead, &event, &answers).await.unwrap();
            assert_eq!((retry.id, retry.submissions), (first.id, 1));
            answers.submission_key = "second".into();
            answers.concern = "Missed services";
            let second = super::record_intake(&lead, &event, &answers).await.unwrap();
            assert_eq!((second.id, second.submissions), (first.id, 2));
            let rows = super::intake_history(first.id, admin, Role::Admin)
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);
            assert!(rows[0].contains("Missed services") && rows[1].contains("Evaluation"));
            answers.submission_key = "contact".into();
            answers.concern = "Review a business agreement";
            answers.details = super::LeadIntakeDetails::Contact {
                name: "Example Visitor",
            };
            super::record_intake(&lead, &event, &answers).await.unwrap();
            let history = super::intake_history(first.id, admin, Role::Admin)
                .await
                .unwrap();
            assert!(history[0].contains("Name: Example Visitor"));
            assert!(history[0].contains("Inquiry: Review a business agreement"));
            assert!(super::intake_history(first.id, Uuid::now_v7(), Role::Admin)
                .await
                .unwrap()
                .is_empty());
            assert!(super::intake_history(first.id, admin, Role::Client)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
        fixture.close().await.unwrap();
    }

    #[tokio::test]
    async fn record_uses_random_ids_and_preserves_identity_on_repeat() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            let input = lead("first@example.test", "synthetic-brand");
            let first = record(&db, &input).await.unwrap();
            let other = record(&db, &lead("second@example.test", "synthetic-brand"))
                .await
                .unwrap();
            let repeated = record(&db, &input).await.unwrap();

            assert_eq!(first.id.get_version_num(), 7);
            assert_eq!(other.id.get_version_num(), 7);
            assert_ne!(first.id, other.id);
            assert_eq!(repeated.id, first.id);
            assert_eq!(repeated.inserted_at, first.inserted_at);
            assert_eq!(repeated.submissions, 2);
        })
        .await;
    }

    #[tokio::test]
    async fn consent_creation_uses_random_ids_and_preserves_the_lead() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            let input = lead("consent@example.test", "synthetic-brand");
            let event = NewConsentEvent {
                consented_at: input.consented_at,
                phone: None,
                source_path: input.source_path.clone(),
                sms_consent_version: "synthetic-disclosure".into(),
                sms_policy_version: "synthetic-policy".into(),
                sms_opt_in: true,
                call_consent_version: "call disclosure".into(),
                call_opt_in: true,
            };
            let first = record_consent_event(&db, &input, &event).await.unwrap();
            let repeated = record_consent_event(&db, &input, &event).await.unwrap();
            let history = consent_history(&db, first.id).await.unwrap();

            assert_eq!(first.id.get_version_num(), 7);
            assert_eq!(repeated.id, first.id);
            assert_eq!(repeated.inserted_at, first.inserted_at);
            assert_eq!(history.len(), 2);
            assert!(history.iter().all(|entry| entry.id.get_version_num() == 7));
            assert!(history.iter().all(|entry| entry.lead_id == first.id));
            assert_ne!(history[0].id, history[1].id);
        })
        .await;
    }

    #[tokio::test]
    async fn consent_order_uses_microsecond_instants_with_uuid_ties() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

        let parent = record(&db, &lead("history@example.test", "synthetic-brand"))
            .await
            .unwrap();
        let older_id = Uuid::parse_str("ffffffff-ffff-7fff-bfff-ffffffffffff").unwrap();
        let newer_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let older_at = "2026-10-21T12:00:00.123456100Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let newer_at = "2026-10-21T12:00:00.123456900Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        for (id, at) in [(older_id, older_at), (newer_id, newer_at)] {
            sqlx::query("INSERT INTO lead_consent (id, lead_id, consented_at, phone, source_path, sms_consent_version, sms_policy_version, sms_opt_in) VALUES ($1,$2,$3,NULL,'/contact','synthetic-disclosure','synthetic-policy',true)")
                .bind(id).bind(parent.id).bind(at).execute(pg.pool()).await.unwrap();
        }
        let history = consent_history(&db, parent.id).await.unwrap();
        assert_eq!(
            history.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![newer_id, older_id]
        );
            assert_eq!(history[0].consented_at, newer_at.trunc_subsecs(6));
            assert_eq!(history[1].consented_at, older_at.trunc_subsecs(6));
        assert!(
            history
                .iter()
                .all(|entry| entry.call_consent_version.is_none() && !entry.call_opt_in),
            "an event from before the callback choice reads as no consent to a call"
        );
        assert_eq!(
            latest_consent_event(&db, parent.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            older_id
        );
        }).await;
    }

    #[tokio::test]
    async fn records_a_lead_without_creating_a_person() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            let written = record(&db, &lead("Visitor@example.com", "neon"))
                .await
                .unwrap();

            assert_eq!(written.email_lower, "visitor@example.com");
            assert_eq!(written.status, "new");
            assert_eq!(written.submissions, 1);
            assert!(crate::persons::find_by_email_ci(&db, "visitor@example.com")
                .await
                .unwrap()
                .is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn consent_events_preserve_each_submission_tuple_and_opt_out() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let pg = crate::postgres::PostgresDb::application().await.unwrap();

            let mut first = lead("visitor@example.com", "neon");
            first.phone = Some("+1 (555) 010-1111".to_string());
            first.source_path = "/services".to_string();
            first.consent_version = "first email disclosure".to_string();
            let first_at = chrono::DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
            record_consent_event(
                &db,
                &first,
                &NewConsentEvent {
                    consented_at: first_at,
                    phone: first.phone.clone(),
                    source_path: first.source_path.clone(),
                    sms_consent_version: "phone disclosure v0".to_string(),
                    sms_policy_version: "2026-01-01".to_string(),
                    sms_opt_in: true,
                    call_consent_version: "call disclosure".into(),
                    call_opt_in: true,
                },
            )
            .await
            .unwrap();
            let mut repeat = lead("VISITOR@example.com", "neon");
            repeat.phone = Some("+1 (555) 010-2222".to_string());
            repeat.source_path = "/contact".to_string();
            repeat.consent_version = "second email disclosure".to_string();
            let second_at = first_at + chrono::Duration::seconds(1);
            record_consent_event(
                &db,
                &repeat,
                &NewConsentEvent {
                    consented_at: second_at,
                    phone: repeat.phone.clone(),
                    source_path: repeat.source_path.clone(),
                    sms_consent_version: "phone disclosure v1".to_string(),
                    sms_policy_version: "2026-09-18".to_string(),
                    sms_opt_in: true,
                    call_consent_version: "call disclosure".into(),
                    call_opt_in: true,
                },
            )
            .await
            .unwrap();
            let mut unchecked = lead("visitor@example.com", "neon");
            unchecked.phone = Some("+1 (555) 010-3333".to_string());
            unchecked.source_path = "/contact".to_string();
            let third_at = second_at + chrono::Duration::seconds(1);
            let written = record_consent_event(
                &db,
                &unchecked,
                &NewConsentEvent {
                    consented_at: third_at,
                    phone: unchecked.phone.clone(),
                    source_path: unchecked.source_path.clone(),
                    sms_consent_version: "phone disclosure v2".to_string(),
                    sms_policy_version: "2026-09-18".to_string(),
                    sms_opt_in: false,
                    call_consent_version: "call disclosure".into(),
                    call_opt_in: false,
                },
            )
            .await
            .unwrap();

            assert_eq!(written.submissions, 3);
            assert_eq!(written.phone.as_deref(), Some("+1 (555) 010-3333"));
            assert_eq!(written.consent_version, "first email disclosure");
            let history = consent_history(&db, written.id).await.unwrap();
            assert_eq!(history.len(), 3);
            assert_eq!(history[0].consented_at, first_at);
            assert_eq!(history[0].phone.as_deref(), Some("+1 (555) 010-1111"));
            assert!(history[0].call_opt_in);
            assert!(!history[2].call_opt_in);
            assert_eq!(history[0].source_path, "/services");
            assert_eq!(history[0].sms_consent_version, "phone disclosure v0");
            assert!(history[0].sms_opt_in);
            assert_eq!(history[1].phone.as_deref(), Some("+1 (555) 010-2222"));
            assert_eq!(history[1].source_path, "/contact");
            assert!(history[1].sms_opt_in);
            assert_eq!(history[2].phone.as_deref(), Some("+1 (555) 010-3333"));
            assert_eq!(history[2].sms_consent_version, "phone disclosure v2");
            assert_eq!(history[2].sms_policy_version, "2026-09-18");
            assert!(!history[2].sms_opt_in);
            assert_eq!(
                latest_consent_event(&db, written.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                history[2].id
            );
            let rejected = sqlx::query("UPDATE lead_consent SET sms_opt_in = false WHERE id = $1")
                .bind(history[0].id)
                .execute(pg.pool())
                .await;
            assert!(rejected.is_err(), "consent events must be immutable");
            assert_eq!(list(&db).await.unwrap().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn reads_a_historical_lead_without_sms_consent_fields() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
        let historical_id = Uuid::now_v7();
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        sqlx::query("INSERT INTO lead (id,email,email_lower,brand_key,source_path,consent_version,consented_at,status,submissions,inserted_at,updated_at) VALUES ($1,'historical@example.com','historical@example.com','neon','/contact','email disclosure',now(),'new',1,now(),now())")
            .bind(historical_id).execute(pg.pool()).await.unwrap();

        let leads = list(&db).await.unwrap();
        assert_eq!(leads.len(), 1);
        let repeated = record(&db, &lead("historical@example.com", "neon"))
            .await
            .unwrap();
        assert_eq!(repeated.id, historical_id);
        assert_eq!(repeated.inserted_at, leads[0].inserted_at);
        }).await;
    }

    #[tokio::test]
    async fn the_same_mailbox_on_two_brands_is_two_leads() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            record(&db, &lead("visitor@example.com", "neon"))
                .await
                .unwrap();
            record(&db, &lead("visitor@example.com", "delete-your-data"))
                .await
                .unwrap();

            assert_eq!(list(&db).await.unwrap().len(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn status_transitions_persist_and_unsubscribe_stamps_once() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            let lead = record(&db, &lead("visitor@example.com", "neon"))
                .await
                .unwrap();

            let contacted = set_status(&db, lead.id, LeadStatus::Contacted)
                .await
                .unwrap();
            assert_eq!(contacted.status, "contacted");
            assert!(contacted.unsubscribed_at.is_none());

            let first = set_status(&db, lead.id, LeadStatus::Unsubscribed)
                .await
                .unwrap();
            assert_eq!(first.status, "unsubscribed");
            let stamped = first.unsubscribed_at.expect("unsubscribe stamps the row");

            let again = set_status(&db, lead.id, LeadStatus::Unsubscribed)
                .await
                .unwrap();
            assert_eq!(again.unsubscribed_at, Some(stamped));

            let declined = set_status(&db, lead.id, LeadStatus::Declined)
                .await
                .unwrap();
            assert_eq!(declined.status, "declined");
            assert_eq!(declined.unsubscribed_at, Some(stamped));
            assert_eq!(
                find(&db, lead.id).await.unwrap().unwrap().unsubscribed_at,
                Some(stamped)
            );

            assert!(matches!(
                set_status(&db, lead.id, LeadStatus::Converted).await,
                Err(LeadError::ConvertedWithoutPerson)
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn conversion_writes_person_id_and_retries_an_unlinked_person() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            let mut incoming = lead("visitor@example.com", "neon");
            incoming.phone = Some("+1 (555) 010-9876".to_string());
            let written = record(&db, &incoming).await.unwrap();

            let converted = convert(&db, written.id).await.unwrap();
            let person_id = converted.person_id.expect("conversion links a person");
            assert_eq!(converted.status, "converted");
            let person = persons::find_by_id(&db, person_id).await.unwrap().unwrap();
            assert_eq!(person.email, "visitor@example.com");
            assert_eq!(person.role, Role::Client);
            assert_eq!(person.phone.as_deref(), Some("+1 (555) 010-9876"));

            let duplicate = record(&db, &lead("visitor@example.com", "delete-your-data"))
                .await
                .unwrap();
            let linked = convert(&db, duplicate.id).await.unwrap();
            assert_eq!(linked.status, "converted");
            assert_eq!(linked.person_id, Some(person_id));
        })
        .await;
    }

    #[tokio::test]
    async fn link_copies_a_missing_phone_onto_the_person_and_leaves_an_existing_number() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            let without_phone =
                persons::create(&db, &NewPerson::new("Existing", "link@example.com"))
                    .await
                    .unwrap();
            let mut incoming = lead("link@example.com", "neon");
            incoming.phone = Some("+1 (555) 010-1111".to_string());
            let written = record(&db, &incoming).await.unwrap();

            link_person(&db, written.id).await.unwrap();
            let person = persons::find_by_id(&db, without_phone.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(person.phone.as_deref(), Some("+1 (555) 010-1111"));

            let mut other = NewPerson::new("Kept", "kept@example.com");
            other.phone = Some("+1 (555) 010-2222".to_string());
            let kept = persons::create(&db, &other).await.unwrap();
            let mut second = lead("kept@example.com", "neon");
            second.phone = Some("+1 (555) 010-3333".to_string());
            let second_lead = record(&db, &second).await.unwrap();
            link_person(&db, second_lead.id).await.unwrap();
            let person = persons::find_by_id(&db, kept.id).await.unwrap().unwrap();
            assert_eq!(person.phone.as_deref(), Some("+1 (555) 010-2222"));
        })
        .await;
    }

    #[tokio::test]
    async fn convert_links_an_already_seeded_mailbox() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();

            persons::create(&db, &NewPerson::new("Existing", "already@example.com"))
                .await
                .unwrap();
            let written = record(&db, &lead("already@example.com", "neon"))
                .await
                .unwrap();
            let linked = convert(&db, written.id).await.unwrap();
            assert_eq!(linked.status, "converted");
            assert!(linked.person_id.is_some());
        })
        .await;
    }

    #[test]
    fn mask_phone_keeps_only_the_last_four_digits() {
        assert_eq!(mask_phone(Some("+1 (555) 010-9876")), "…9876");
        assert_eq!(mask_phone(None), "—");
        assert_eq!(mask_phone(Some("12")), "…12");
    }
}
