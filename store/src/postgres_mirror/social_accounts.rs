//! Best-effort mirror of encrypted Social Account connections.

use super::{changed, compare, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use std::collections::BTreeMap;
use surrealdb::types::{Datetime, RecordId, RecordIdKey, SurrealValue};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SocialAccountImage {
    id: String,
    firm_id: uuid::Uuid,
    brand_key: String,
    provider: String,
    provider_account_id: String,
    display_name: Option<String>,
    scopes: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
    status: String,
    connected_by_person_id: uuid::Uuid,
    revoked_by_person_id: Option<uuid::Uuid>,
    revoked_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for SocialAccountImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocialAccountImage")
            .field("id", &self.id)
            .field("firm_id", &self.firm_id)
            .field("brand_key", &self.brand_key)
            .field("provider", &self.provider)
            .field("provider_account_id", &self.provider_account_id)
            .field("display_name", &self.display_name)
            .field("scopes", &self.scopes)
            .field("expires_at", &self.expires_at)
            .field(
                "has_encrypted_bundle",
                &(!self.ciphertext.is_empty() && !self.wrapped_dek.is_empty()),
            )
            .field("kms_key_version", &self.kms_key_version)
            .field("kms_context", &"[REDACTED]")
            .field("status", &self.status)
            .field("connected_by_person_id", &self.connected_by_person_id)
            .field("revoked_by_person_id", &self.revoked_by_person_id)
            .field("revoked_at", &self.revoked_at)
            .field("inserted_at", &self.inserted_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(SurrealValue)]
struct Source {
    id: RecordId,
    firm_id: RecordId,
    brand_key: String,
    provider: String,
    provider_account_id: String,
    display_name: Option<String>,
    scopes: Vec<String>,
    expires_at: Option<Datetime>,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
    status: String,
    connected_by_person_id: RecordId,
    revoked_by_person_id: Option<RecordId>,
    revoked_at: Option<Datetime>,
    inserted_at: Datetime,
    updated_at: Datetime,
}

impl TryFrom<Source> for SocialAccountImage {
    type Error = MirrorError;
    fn try_from(row: Source) -> Result<Self, Self::Error> {
        Ok(Self {
            id: string_key(&row.id)?,
            firm_id: record_uuid(&row.firm_id).ok_or(MirrorError::Source)?,
            brand_key: row.brand_key,
            provider: row.provider,
            provider_account_id: row.provider_account_id,
            display_name: row.display_name,
            scopes: row.scopes,
            expires_at: row.expires_at.map(Into::into).map(micros),
            ciphertext: row.ciphertext,
            wrapped_dek: row.wrapped_dek,
            kms_key_version: row.kms_key_version,
            kms_context: row.kms_context,
            status: row.status,
            connected_by_person_id: record_uuid(&row.connected_by_person_id)
                .ok_or(MirrorError::Source)?,
            revoked_by_person_id: row
                .revoked_by_person_id
                .as_ref()
                .map(|id| record_uuid(id).ok_or(MirrorError::Source))
                .transpose()?,
            revoked_at: row.revoked_at.map(Into::into).map(micros),
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}

fn string_key(id: &RecordId) -> Result<String, MirrorError> {
    match &id.key {
        RecordIdKey::String(key) if id.table.as_str() == "social_account" => Ok(key.clone()),
        _ => Err(MirrorError::Source),
    }
}

const SELECT: &str = "id, firm_id, brand_key, provider, provider_account_id, display_name, scopes, expires_at, ciphertext, wrapped_dek, kms_key_version, kms_context, status, connected_by_person_id, revoked_by_person_id, revoked_at, inserted_at, updated_at";

async fn source(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<Source> = surreal
        .query(format!("SELECT {SELECT} FROM social_account"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = SocialAccountImage::try_from(row)?;
            Ok((
                MirrorKey::SocialAccount(image.id.clone()),
                Columns::SocialAccount(Box::new(image)),
            ))
        })
        .collect()
}

async fn source_one(
    surreal: &SurrealDb,
    key: &str,
) -> Result<Option<SocialAccountImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", RecordId::new("social_account", key.to_string())))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<Source> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(SocialAccountImage::try_from).transpose()
}

fn from_row(row: &PgRow) -> Result<SocialAccountImage, sqlx::Error> {
    Ok(SocialAccountImage {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        brand_key: row.try_get("brand_key")?,
        provider: row.try_get("provider")?,
        provider_account_id: row.try_get("provider_account_id")?,
        display_name: row.try_get("display_name")?,
        scopes: row.try_get("scopes")?,
        expires_at: row.try_get("expires_at")?,
        ciphertext: row.try_get("ciphertext")?,
        wrapped_dek: row.try_get("wrapped_dek")?,
        kms_key_version: row.try_get("kms_key_version")?,
        kms_context: row.try_get("kms_context")?,
        status: row.try_get("status")?,
        connected_by_person_id: row.try_get("connected_by_person_id")?,
        revoked_by_person_id: row.try_get("revoked_by_person_id")?,
        revoked_at: row.try_get("revoked_at")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    sqlx::query("SELECT id, firm_id, brand_key, provider, provider_account_id, display_name, scopes, expires_at, ciphertext, wrapped_dek, kms_key_version, kms_context, status, connected_by_person_id, revoked_by_person_id, revoked_at, inserted_at, updated_at FROM social_account")
        .fetch_all(&pg.0).await?.into_iter().map(|row| {
            let image = from_row(&row)?;
            Ok((MirrorKey::SocialAccount(image.id.clone()), Columns::SocialAccount(Box::new(image))))
        }).collect()
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &str,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = source_one(surreal, key).await? else {
        let result = sqlx::query("DELETE FROM social_account WHERE id=$1")
            .bind(key)
            .execute(&pg.0)
            .await?;
        return Ok(changed(result.rows_affected(), Mirrored::Deleted));
    };
    super::tenancy::mirror(surreal, pg, &MirrorKey::Firm(row.firm_id)).await?;
    let brand = crate::brands::find_by_key(surreal, &row.brand_key)
        .await
        .map_err(|_| MirrorError::Source)?
        .ok_or(MirrorError::Source)?;
    if brand.firm_id != Some(row.firm_id) {
        return Err(MirrorError::Source);
    }
    super::tenancy::mirror(surreal, pg, &MirrorKey::Brand(brand.id)).await?;
    super::identity::mirror(surreal, pg, &MirrorKey::Person(row.connected_by_person_id)).await?;
    if let Some(person_id) = row.revoked_by_person_id {
        super::identity::mirror(surreal, pg, &MirrorKey::Person(person_id)).await?;
    }
    let result = sqlx::query("INSERT INTO social_account (id, firm_id, brand_key, provider, provider_account_id, display_name, scopes, expires_at, ciphertext, wrapped_dek, kms_key_version, kms_context, status, connected_by_person_id, revoked_by_person_id, revoked_at, inserted_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) ON CONFLICT (id) DO UPDATE SET firm_id=EXCLUDED.firm_id, brand_key=EXCLUDED.brand_key, provider=EXCLUDED.provider, provider_account_id=EXCLUDED.provider_account_id, display_name=EXCLUDED.display_name, scopes=EXCLUDED.scopes, expires_at=EXCLUDED.expires_at, ciphertext=EXCLUDED.ciphertext, wrapped_dek=EXCLUDED.wrapped_dek, kms_key_version=EXCLUDED.kms_key_version, kms_context=EXCLUDED.kms_context, status=EXCLUDED.status, connected_by_person_id=EXCLUDED.connected_by_person_id, revoked_by_person_id=EXCLUDED.revoked_by_person_id, revoked_at=EXCLUDED.revoked_at, inserted_at=EXCLUDED.inserted_at, updated_at=EXCLUDED.updated_at WHERE (social_account.firm_id,social_account.brand_key,social_account.provider,social_account.provider_account_id,social_account.display_name,social_account.scopes,social_account.expires_at,social_account.ciphertext,social_account.wrapped_dek,social_account.kms_key_version,social_account.kms_context,social_account.status,social_account.connected_by_person_id,social_account.revoked_by_person_id,social_account.revoked_at,social_account.inserted_at,social_account.updated_at) IS DISTINCT FROM (EXCLUDED.firm_id,EXCLUDED.brand_key,EXCLUDED.provider,EXCLUDED.provider_account_id,EXCLUDED.display_name,EXCLUDED.scopes,EXCLUDED.expires_at,EXCLUDED.ciphertext,EXCLUDED.wrapped_dek,EXCLUDED.kms_key_version,EXCLUDED.kms_context,EXCLUDED.status,EXCLUDED.connected_by_person_id,EXCLUDED.revoked_by_person_id,EXCLUDED.revoked_at,EXCLUDED.inserted_at,EXCLUDED.updated_at)")
        .bind(&row.id).bind(row.firm_id).bind(&row.brand_key).bind(&row.provider).bind(&row.provider_account_id).bind(&row.display_name).bind(&row.scopes).bind(row.expires_at).bind(&row.ciphertext).bind(&row.wrapped_dek).bind(&row.kms_key_version).bind(&row.kms_context).bind(&row.status).bind(row.connected_by_person_id).bind(row.revoked_by_person_id).bind(row.revoked_at).bind(row.inserted_at).bind(row.updated_at).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![compare(
            "social_account",
            &source(surreal).await?,
            &target(pg).await?,
        )],
    })
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source(surreal).await?;
    let target = target(pg).await?;
    for key in source
        .keys()
        .filter(|key| target.get(*key) != source.get(*key))
    {
        if let MirrorKey::SocialAccount(key) = key {
            mirror(surreal, pg, key).await?;
        }
    }
    for key in target.keys().filter(|key| !source.contains_key(*key)) {
        if let MirrorKey::SocialAccount(key) = key {
            mirror(surreal, pg, key).await?;
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "social_accounts_tests.rs"]
mod tests;
