//! PostgreSQL mirror for durable on-chain Notation attestations.

use std::{collections::BTreeMap, sync::LazyLock};

use chrono::{DateTime, Utc};
use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use super::tenancy::{compare, remirror, select_sql, target, timestamp, upsert_sql};
use super::{changed, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_id, record_uuid, SurrealDb},
};

const COLUMNS: &[&str] = &[
    "id",
    "notation_id",
    "chain",
    "sha256",
    "status",
    "pda",
    "tx_signature",
    "firm_wallet",
    "client_wallet",
    "recorded_at",
    "inserted_at",
    "updated_at",
];
const SOURCE_FIELDS: &str = "id, notation_id, chain, sha256, status, pda, tx_signature, \
    firm_wallet, client_wallet, recorded_at, inserted_at, updated_at";
static UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("attestation", COLUMNS));
static SELECT: LazyLock<String> = LazyLock::new(|| select_sql("attestation", COLUMNS));

#[derive(SurrealValue)]
struct SourceRow {
    id: surrealdb::types::RecordId,
    notation_id: surrealdb::types::RecordId,
    chain: String,
    sha256: String,
    status: String,
    pda: Option<String>,
    tx_signature: Option<String>,
    firm_wallet: Option<String>,
    client_wallet: Option<String>,
    recorded_at: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Image {
    id: Uuid,
    notation_id: Uuid,
    chain: String,
    sha256: String,
    status: String,
    pda: Option<String>,
    tx_signature: Option<String>,
    firm_wallet: Option<String>,
    client_wallet: Option<String>,
    recorded_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SourceRow {
    fn image(self) -> Result<Image, MirrorError> {
        Ok(Image {
            id: record_uuid(&self.id).ok_or(MirrorError::Source)?,
            notation_id: record_uuid(&self.notation_id).ok_or(MirrorError::Source)?,
            chain: self.chain,
            sha256: self.sha256,
            status: self.status,
            pda: self.pda,
            tx_signature: self.tx_signature,
            firm_wallet: self.firm_wallet,
            client_wallet: self.client_wallet,
            recorded_at: self.recorded_at.as_deref().map(timestamp).transpose()?,
            inserted_at: super::micros(self.inserted_at.into()),
            updated_at: super::micros(self.updated_at.into()),
        })
    }
}

impl Image {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            notation_id: row.try_get("notation_id")?,
            chain: row.try_get("chain")?,
            sha256: row.try_get("sha256")?,
            status: row.try_get("status")?,
            pda: row.try_get("pda")?,
            tx_signature: row.try_get("tx_signature")?,
            firm_wallet: row.try_get("firm_wallet")?,
            client_wallet: row.try_get("client_wallet")?,
            recorded_at: row.try_get("recorded_at")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

async fn rows(surreal: &SurrealDb) -> Result<Vec<SourceRow>, MirrorError> {
    let mut result = surreal
        .query(format!("SELECT {SOURCE_FIELDS} FROM attestation"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    result.take(0).map_err(|_| MirrorError::Source)
}

pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let mut result = surreal
        .query(format!("SELECT {SOURCE_FIELDS} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id("attestation", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<SourceRow> = result.take(0).map_err(|_| MirrorError::Source)?;
    let Some(row) = row else {
        let deleted = sqlx::query("DELETE FROM attestation WHERE id=$1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    let image = row.image()?;
    super::workflow::mirror(surreal, pg, &MirrorKey::Notation(image.notation_id)).await?;
    let written = sqlx::query(UPSERT.as_str())
        .bind(image.id)
        .bind(image.notation_id)
        .bind(&image.chain)
        .bind(&image.sha256)
        .bind(&image.status)
        .bind(&image.pda)
        .bind(&image.tx_signature)
        .bind(&image.firm_wallet)
        .bind(&image.client_wallet)
        .bind(image.recorded_at)
        .bind(image.inserted_at)
        .bind(image.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

pub(super) async fn parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<ParityReport, MirrorError> {
    let mut source = BTreeMap::new();
    for row in rows(surreal).await? {
        let image = row.image()?;
        source.insert(MirrorKey::Attestation(image.id), Some(image));
    }
    let target = target(pg, SELECT.as_str(), Image::from_row, |row| {
        MirrorKey::Attestation(row.id)
    })
    .await?;
    let mut report = compare("attestation", &source, &target);
    // A durable source attestation whose source Notation disappeared is not
    // eligible for mirroring; keep the missing dependency visible and retain it.
    for (key, image) in &source {
        if let Some(image) = image {
            let mut result = surreal
                .query("SELECT id FROM ONLY $id LIMIT 1")
                .bind(("id", record_id(crate::notations::TABLE, image.notation_id)))
                .await
                .and_then(surrealdb::IndexedResults::check)
                .map_err(|_| MirrorError::Source)?;
            let source_parent: Option<surrealdb::types::RecordId> =
                result.take(0).map_err(|_| MirrorError::Source)?;
            let present = source_parent.is_some();
            if !present && !report.divergent.contains(key) {
                report.divergent.push(key.clone());
            }
        }
    }
    report.divergent.sort();
    report.divergent.dedup();
    Ok(ParityReport {
        tables: vec![report],
    })
}

pub(super) async fn reconcile(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "attestations_tests.rs"]
mod tests;
