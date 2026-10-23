//! PostgreSQL mirror for public Leads and their immutable SMS-consent history.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LeadImage {
    id: Uuid,
    email: String,
    email_lower: String,
    phone: Option<String>,
    brand_key: String,
    source_path: String,
    consent_version: String,
    consented_at: DateTime<Utc>,
    status: String,
    unsubscribed_at: Option<DateTime<Utc>>,
    person_id: Option<Uuid>,
    submissions: i64,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConsentImage {
    id: Uuid,
    lead_id: Uuid,
    consented_at: DateTime<Utc>,
    phone: Option<String>,
    source_path: String,
    sms_consent_version: String,
    sms_policy_version: String,
    sms_opt_in: bool,
}

#[derive(SurrealValue)]
struct LeadSource {
    id: RecordId,
    email: String,
    email_lower: String,
    phone: Option<String>,
    brand_key: String,
    source_path: String,
    consent_version: String,
    consented_at: Datetime,
    status: String,
    unsubscribed_at: Option<Datetime>,
    person_id: Option<RecordId>,
    submissions: i64,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(SurrealValue)]
struct ConsentSource {
    id: RecordId,
    lead_id: RecordId,
    consented_at: Datetime,
    phone: Option<String>,
    source_path: String,
    sms_consent_version: String,
    sms_policy_version: String,
    sms_opt_in: bool,
}

const LEAD_FIELDS: &str = "id, email, email_lower, phone, brand_key, source_path, consent_version, consented_at, status, unsubscribed_at, person_id, submissions, inserted_at, updated_at";
const CONSENT_FIELDS: &str = "id, lead_id, consented_at, phone, source_path, sms_consent_version, sms_policy_version, sms_opt_in";

fn uuid(id: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(id).ok_or(MirrorError::Source)
}

fn optional_uuid(id: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    id.map(uuid).transpose()
}

impl TryFrom<LeadSource> for LeadImage {
    type Error = MirrorError;
    fn try_from(row: LeadSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            email: row.email,
            email_lower: row.email_lower,
            phone: row.phone,
            brand_key: row.brand_key,
            source_path: row.source_path,
            consent_version: row.consent_version,
            consented_at: micros(row.consented_at.into()),
            status: row.status,
            unsubscribed_at: row.unsubscribed_at.map(Into::into).map(micros),
            person_id: optional_uuid(row.person_id.as_ref())?,
            submissions: row.submissions,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}

impl TryFrom<ConsentSource> for ConsentImage {
    type Error = MirrorError;
    fn try_from(row: ConsentSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            lead_id: uuid(&row.lead_id)?,
            consented_at: micros(row.consented_at.into()),
            phone: row.phone,
            source_path: row.source_path,
            sms_consent_version: row.sms_consent_version,
            sms_policy_version: row.sms_policy_version,
            sms_opt_in: row.sms_opt_in,
        })
    }
}

async fn source_leads(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<LeadSource> = surreal
        .query(format!("SELECT {LEAD_FIELDS} FROM lead"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = LeadImage::try_from(row)?;
            Ok((MirrorKey::Lead(image.id), Columns::Lead(Box::new(image))))
        })
        .collect()
}

async fn source_consents(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<ConsentSource> = surreal
        .query(format!("SELECT {CONSENT_FIELDS} FROM lead_consent"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = ConsentImage::try_from(row)?;
            Ok((
                MirrorKey::LeadConsent(image.id),
                Columns::LeadConsent(Box::new(image)),
            ))
        })
        .collect()
}

async fn source_lead(surreal: &SurrealDb, id: Uuid) -> Result<Option<LeadImage>, MirrorError> {
    let mut rows = surreal
        .query(format!("SELECT {LEAD_FIELDS} FROM ONLY $id"))
        .bind(("id", crate::surreal::record_id("lead", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<LeadSource> = rows.take(0).map_err(|_| MirrorError::Source)?;
    row.map(LeadImage::try_from).transpose()
}

async fn source_consent(
    surreal: &SurrealDb,
    id: Uuid,
) -> Result<Option<ConsentImage>, MirrorError> {
    let mut rows = surreal
        .query(format!("SELECT {CONSENT_FIELDS} FROM ONLY $id"))
        .bind(("id", crate::surreal::record_id("lead_consent", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<ConsentSource> = rows.take(0).map_err(|_| MirrorError::Source)?;
    row.map(ConsentImage::try_from).transpose()
}

async fn target_leads(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT id,email,email_lower,phone,brand_key,source_path,consent_version,consented_at,status,unsubscribed_at,person_id,submissions,inserted_at,updated_at FROM lead")
        .fetch_all(&pg.0).await?;
    rows.into_iter()
        .map(|row| {
            let image = lead_from_row(&row)?;
            Ok((MirrorKey::Lead(image.id), Columns::Lead(Box::new(image))))
        })
        .collect()
}

async fn target_consents(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT id,lead_id,consented_at,phone,source_path,sms_consent_version,sms_policy_version,sms_opt_in FROM lead_consent")
        .fetch_all(&pg.0).await?;
    rows.into_iter()
        .map(|row| {
            let image = consent_from_row(&row)?;
            Ok((
                MirrorKey::LeadConsent(image.id),
                Columns::LeadConsent(Box::new(image)),
            ))
        })
        .collect()
}

fn lead_from_row(row: &PgRow) -> Result<LeadImage, sqlx::Error> {
    Ok(LeadImage {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        email_lower: row.try_get("email_lower")?,
        phone: row.try_get("phone")?,
        brand_key: row.try_get("brand_key")?,
        source_path: row.try_get("source_path")?,
        consent_version: row.try_get("consent_version")?,
        consented_at: row.try_get("consented_at")?,
        status: row.try_get("status")?,
        unsubscribed_at: row.try_get("unsubscribed_at")?,
        person_id: row.try_get("person_id")?,
        submissions: row.try_get("submissions")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn consent_from_row(row: &PgRow) -> Result<ConsentImage, sqlx::Error> {
    Ok(ConsentImage {
        id: row.try_get("id")?,
        lead_id: row.try_get("lead_id")?,
        consented_at: row.try_get("consented_at")?,
        phone: row.try_get("phone")?,
        source_path: row.try_get("source_path")?,
        sms_consent_version: row.try_get("sms_consent_version")?,
        sms_policy_version: row.try_get("sms_policy_version")?,
        sms_opt_in: row.try_get("sms_opt_in")?,
    })
}

async fn upsert_lead(pg: &PostgresDb, row: &LeadImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query("INSERT INTO lead (id,email,email_lower,phone,brand_key,source_path,consent_version,consented_at,status,unsubscribed_at,person_id,submissions,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT (id) DO UPDATE SET email=EXCLUDED.email,email_lower=EXCLUDED.email_lower,phone=EXCLUDED.phone,brand_key=EXCLUDED.brand_key,source_path=EXCLUDED.source_path,consent_version=EXCLUDED.consent_version,consented_at=EXCLUDED.consented_at,status=EXCLUDED.status,unsubscribed_at=EXCLUDED.unsubscribed_at,person_id=EXCLUDED.person_id,submissions=EXCLUDED.submissions,inserted_at=EXCLUDED.inserted_at,updated_at=EXCLUDED.updated_at WHERE (lead.email,lead.email_lower,lead.phone,lead.brand_key,lead.source_path,lead.consent_version,lead.consented_at,lead.status,lead.unsubscribed_at,lead.person_id,lead.submissions,lead.inserted_at,lead.updated_at) IS DISTINCT FROM (EXCLUDED.email,EXCLUDED.email_lower,EXCLUDED.phone,EXCLUDED.brand_key,EXCLUDED.source_path,EXCLUDED.consent_version,EXCLUDED.consented_at,EXCLUDED.status,EXCLUDED.unsubscribed_at,EXCLUDED.person_id,EXCLUDED.submissions,EXCLUDED.inserted_at,EXCLUDED.updated_at)")
        .bind(row.id).bind(&row.email).bind(&row.email_lower).bind(&row.phone).bind(&row.brand_key).bind(&row.source_path)
        .bind(&row.consent_version).bind(micros(row.consented_at)).bind(&row.status).bind(row.unsubscribed_at.map(micros))
        .bind(row.person_id).bind(row.submissions).bind(micros(row.inserted_at)).bind(micros(row.updated_at)).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn insert_consent(pg: &PostgresDb, row: &ConsentImage) -> Result<Mirrored, MirrorError> {
    // The UUID is stable source identity. Conflicts preserve the first stored
    // evidence, whether it matches or needs operator review in parity.
    let result = sqlx::query("INSERT INTO lead_consent (id,lead_id,consented_at,phone,source_path,sms_consent_version,sms_policy_version,sms_opt_in) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (id) DO NOTHING")
        .bind(row.id).bind(row.lead_id).bind(micros(row.consented_at)).bind(&row.phone).bind(&row.source_path)
        .bind(&row.sms_consent_version).bind(&row.sms_policy_version).bind(row.sms_opt_in).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn mirror_lead(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = source_lead(surreal, id).await? else {
        // Consent rows are independent immutable evidence. Removing this
        // projection never cascades into the evidence ledger.
        return Ok(changed(
            sqlx::query("DELETE FROM lead WHERE id=$1")
                .bind(id)
                .execute(&pg.0)
                .await?
                .rows_affected(),
            Mirrored::Deleted,
        ));
    };
    upsert_lead(pg, &row).await
}

async fn mirror_consent(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = source_consent(surreal, id).await? else {
        // Extra target events remain visible to parity; the evidence ledger is
        // append-only on both sides.
        return Ok(Mirrored::Unchanged);
    };
    if source_lead(surreal, row.lead_id).await?.is_none() {
        return Err(MirrorError::Source);
    }
    mirror_lead(surreal, pg, row.lead_id).await?;
    insert_consent(pg, &row).await
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Lead(id) => mirror_lead(surreal, pg, *id).await,
        MirrorKey::LeadConsent(id) => mirror_consent(surreal, pg, *id).await,
        _ => Err(MirrorError::Source),
    }
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![
            super::compare(
                "lead",
                &source_leads(surreal).await?,
                &target_leads(pg).await?,
            ),
            super::compare(
                "lead_consent",
                &source_consents(surreal).await?,
                &target_consents(pg).await?,
            ),
        ],
    })
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source_leads(surreal).await?;
    let target = target_leads(pg).await?;
    let keys: Vec<_> = source
        .iter()
        .filter(|(key, image)| target.get(*key) != Some(*image))
        .map(|(key, _)| key.clone())
        .chain(
            target
                .keys()
                .filter(|key| !source.contains_key(*key))
                .cloned(),
        )
        .collect();
    for key in keys {
        mirror(surreal, pg, &key).await?;
    }
    // Reconcile can add absent events; it deliberately leaves modified and
    // extra events intact for explicit audit review.
    let source = source_consents(surreal).await?;
    let target = target_consents(pg).await?;
    for (key, columns) in source.iter().filter(|(key, _)| !target.contains_key(*key)) {
        let Columns::LeadConsent(event) = columns else {
            continue;
        };
        if source_lead(surreal, event.lead_id).await?.is_some() {
            mirror(surreal, pg, key).await?;
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "leads_tests.rs"]
mod tests;
