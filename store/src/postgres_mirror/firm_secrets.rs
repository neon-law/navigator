//! Best-effort mirror of encrypted Firm integration-secret versions.

use super::{MirrorError, MirrorKey, Mirrored, ParityReport, TableParity};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::collections::BTreeMap;
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
struct Image {
    id: Uuid,
    firm_id: Uuid,
    provider: String,
    kind: String,
    version: i64,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
    status: String,
    actor_id: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for SecretDebug<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirmIntegrationSecret")
            .field("id", &self.0.id)
            .field("firm_id", &self.0.firm_id)
            .field("provider", &self.0.provider)
            .field("kind", &self.0.kind)
            .field("version", &self.0.version)
            .field("has_ciphertext", &!self.0.ciphertext.is_empty())
            .field("has_wrapped_dek", &!self.0.wrapped_dek.is_empty())
            .field("kms_key_version", &self.0.kms_key_version)
            .field("kms_context", &"[REDACTED]")
            .field("status", &self.0.status)
            .field("actor_id", &self.0.actor_id)
            .field("created_at", &self.0.created_at)
            .field("updated_at", &self.0.updated_at)
            .finish()
    }
}
struct SecretDebug<'a>(&'a Image);
impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        SecretDebug(self).fmt(f)
    }
}

#[derive(SurrealValue)]
struct Source {
    id: RecordId,
    firm_id: RecordId,
    provider: String,
    kind: String,
    version: i64,
    ciphertext: String,
    wrapped_dek: String,
    kms_key_version: String,
    kms_context: String,
    status: String,
    actor_id: RecordId,
    created_at: String,
    updated_at: String,
}

fn uuid(id: &RecordId, table: &str) -> Result<Uuid, MirrorError> {
    if id.table.as_str() != table {
        return Err(MirrorError::Source);
    }
    record_uuid(id).ok_or(MirrorError::Source)
}

fn image(row: Source) -> Result<Image, MirrorError> {
    Ok(Image {
        id: uuid(&row.id, "firm_integration_secret")?,
        firm_id: uuid(&row.firm_id, "firm")?,
        provider: row.provider,
        kind: row.kind,
        version: row.version,
        ciphertext: row.ciphertext,
        wrapped_dek: row.wrapped_dek,
        kms_key_version: row.kms_key_version,
        kms_context: row.kms_context,
        status: row.status,
        actor_id: uuid(&row.actor_id, "person")?,
        created_at: super::tenancy::timestamp(&row.created_at)?,
        updated_at: super::tenancy::timestamp(&row.updated_at)?,
    })
}

async fn source(surreal: &SurrealDb, firm_id: Uuid) -> Result<BTreeMap<Uuid, Image>, MirrorError> {
    let rows: Vec<Source> = surreal.query("SELECT id, firm_id, provider, kind, version, ciphertext, wrapped_dek, kms_key_version, kms_context, status, actor_id, created_at, updated_at FROM firm_integration_secret WHERE firm_id = $firm_id")
        .bind(("firm_id", crate::surreal::record_id("firm", firm_id))).await
        .and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?
        .take(0).map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let row = image(row)?;
            Ok((row.id, row))
        })
        .collect()
}

async fn target(pg: &PostgresDb, firm_id: Uuid) -> Result<BTreeMap<Uuid, Image>, MirrorError> {
    sqlx::query("SELECT id, firm_id, provider, kind, version, ciphertext, wrapped_dek, kms_key_version, kms_context, status, actor_id, created_at, updated_at FROM firm_integration_secret WHERE firm_id=$1")
        .bind(firm_id).fetch_all(&pg.0).await?.into_iter().map(|r| Ok((r.try_get("id")?, Image {
            id: r.try_get("id")?, firm_id: r.try_get("firm_id")?, provider: r.try_get("provider")?, kind: r.try_get("kind")?, version: r.try_get("version")?, ciphertext: r.try_get("ciphertext")?, wrapped_dek: r.try_get("wrapped_dek")?, kms_key_version: r.try_get("kms_key_version")?, kms_context: r.try_get("kms_context")?, status: r.try_get("status")?, actor_id: r.try_get("actor_id")?, created_at: r.try_get("created_at")?, updated_at: r.try_get("updated_at")?,
        }))).collect()
}

async fn upsert(pg: &PostgresDb, r: &Image) -> Result<(), MirrorError> {
    sqlx::query("INSERT INTO firm_integration_secret (id,firm_id,provider,kind,version,ciphertext,wrapped_dek,kms_key_version,kms_context,status,actor_id,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT (firm_id,provider,kind,version) DO UPDATE SET id=EXCLUDED.id,ciphertext=EXCLUDED.ciphertext,wrapped_dek=EXCLUDED.wrapped_dek,kms_key_version=EXCLUDED.kms_key_version,kms_context=EXCLUDED.kms_context,status=EXCLUDED.status,actor_id=EXCLUDED.actor_id,created_at=EXCLUDED.created_at,updated_at=EXCLUDED.updated_at WHERE (firm_integration_secret.id,firm_integration_secret.ciphertext,firm_integration_secret.wrapped_dek,firm_integration_secret.kms_key_version,firm_integration_secret.kms_context,firm_integration_secret.status,firm_integration_secret.actor_id,firm_integration_secret.created_at,firm_integration_secret.updated_at) IS DISTINCT FROM (EXCLUDED.id,EXCLUDED.ciphertext,EXCLUDED.wrapped_dek,EXCLUDED.kms_key_version,EXCLUDED.kms_context,EXCLUDED.status,EXCLUDED.actor_id,EXCLUDED.created_at,EXCLUDED.updated_at)")
        .bind(r.id).bind(r.firm_id).bind(&r.provider).bind(&r.kind).bind(r.version).bind(&r.ciphertext).bind(&r.wrapped_dek).bind(&r.kms_key_version).bind(&r.kms_context).bind(&r.status).bind(r.actor_id).bind(r.created_at).bind(r.updated_at).execute(&pg.0).await?;
    Ok(())
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let rows = source(surreal, firm_id).await?;
    for row in rows.values() {
        super::identity::mirror(surreal, pg, &MirrorKey::Person(row.actor_id)).await?;
    }
    if !rows.is_empty() {
        super::tenancy::mirror(surreal, pg, &MirrorKey::Firm(firm_id)).await?;
    }
    let src = rows;
    let dst = target(pg, firm_id).await?;
    let mut changed = false;
    for (id, row) in &src {
        if dst.get(id) != Some(row) {
            upsert(pg, row).await?;
            changed = true;
        }
    }
    for id in dst.keys().filter(|id| !src.contains_key(id)) {
        sqlx::query("DELETE FROM firm_integration_secret WHERE id=$1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        changed = true;
    }
    Ok(if changed {
        Mirrored::Written
    } else {
        Mirrored::Unchanged
    })
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    compare_all(surreal, pg).await
}
async fn compare_all(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source_ids: Vec<RecordId> = surreal
        .query("SELECT VALUE id FROM firm")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    let source_secret_firms: Vec<RecordId> = surreal
        .query("SELECT VALUE firm_id FROM firm_integration_secret")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    let target_firms: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT firm_id FROM firm_integration_secret ORDER BY firm_id")
            .fetch_all(&pg.0)
            .await?;
    let ids = source_ids
        .into_iter()
        .chain(source_secret_firms)
        .map(|id| uuid(&id, "firm"))
        .chain(target_firms.into_iter().map(Ok))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let mut divergent = Vec::new();
    let mut source_rows = 0;
    let mut target_rows = 0;
    for firm_id in ids {
        let s = source(surreal, firm_id).await?;
        let t = target(pg, firm_id).await?;
        source_rows += s.len();
        target_rows += t.len();
        if s != t {
            divergent.push(MirrorKey::FirmIntegrationSecrets(firm_id));
        }
    }
    Ok(ParityReport {
        tables: vec![TableParity {
            table: "firm_integration_secret",
            source_rows,
            target_rows,
            divergent,
        }],
    })
}
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let report = compare_all(surreal, pg).await?;
    for key in report.divergent().cloned().collect::<Vec<_>>() {
        if let MirrorKey::FirmIntegrationSecrets(id) = key {
            mirror(surreal, pg, id).await?;
        }
    }
    compare_all(surreal, pg).await
}

#[cfg(test)]
#[path = "firm_secrets_tests.rs"]
mod tests;
