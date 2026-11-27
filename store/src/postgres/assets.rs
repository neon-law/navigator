//! Canonical Asset metadata and the Notation parent locks reset relies on.

use chrono::Utc;
use cloud::StorageService;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

use super::PostgresDb;
use crate::assets::{Asset, AssetError};
use crate::postgres::matter;

fn readable_optional<T: DeserializeOwned>(value: &mut Value, name: &str) {
    if value.get(name).is_some_and(|field| {
        !field.is_null() && serde_json::from_value::<T>(field.clone()).is_err()
    }) {
        value[name] = Value::Null;
    }
}

pub(crate) fn decode_value(mut value: Value) -> Result<Asset, AssetError> {
    readable_optional::<crate::documents::Signing>(&mut value, "signing");
    readable_optional::<crate::documents::Evidence>(&mut value, "evidence");
    if value.get("notarization").is_none_or(|field| {
        serde_json::from_value::<Vec<crate::documents::Notarization>>(field.clone()).is_err()
    }) {
        value["notarization"] = json!([]);
    }
    serde_json::from_value(value).map_err(|_| AssetError::WriteReturnedNothing)
}

/// The suffix is a source-code literal; values enter only through the JSON parameter.
pub(crate) async fn rows(
    pg: &PostgresDb,
    suffix: &'static str,
    filter: Value,
) -> Result<Vec<Asset>, AssetError> {
    require_ready(pg).await?;
    let sql = format!(
        "SELECT {} AS row FROM asset r WHERE $1::jsonb IS NOT NULL {suffix}",
        matter::row_json("asset")?
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(filter)
        .fetch_all(pg.pool())
        .await?;
    rows.iter()
        .map(|row| decode_value(row.try_get("row")?))
        .collect()
}

pub(crate) fn notation_parent(key: &str) -> Option<Uuid> {
    let (id, _) = key.strip_prefix("notations/")?.split_once('/')?;
    let parsed = Uuid::parse_str(id).ok()?;
    (parsed.to_string() == id).then_some(parsed)
}

fn notation_parents(row: &Value) -> std::collections::BTreeSet<Uuid> {
    ["storage_key", "secondary_storage_key"]
        .iter()
        .filter_map(|field| row.get(*field)?.as_str().and_then(notation_parent))
        .collect()
}

async fn lock_keys(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    prior: Option<&Value>,
    next: &Value,
) -> Result<(), AssetError> {
    let existing = prior.map(notation_parents).unwrap_or_default();
    let next = notation_parents(next);
    for parent in existing.union(&next).copied() {
        if existing.contains(&parent) {
            // Editing an existing historical orphan does not invent a parent.
            // When it exists, this still serializes with reset.
            sqlx::query("SELECT id FROM notation WHERE id=$1 FOR KEY SHARE")
                .bind(parent)
                .fetch_optional(&mut **tx)
                .await?;
        } else {
            super::notations::lock_parent(tx, parent).await?;
        }
    }
    Ok(())
}

/// Patch metadata in the transaction that holds its canonical parent locks.
pub(crate) async fn patch(
    pg: &PostgresDb,
    id: Uuid,
    changes: Value,
) -> Result<Option<Asset>, AssetError> {
    patch_with_json_clears(pg, id, changes, &[]).await
}

pub(crate) async fn patch_with_json_clears(
    pg: &PostgresDb,
    id: Uuid,
    changes: Value,
    clear: &[&str],
) -> Result<Option<Asset>, AssetError> {
    let mut tx = pg.pool().begin().await?;
    let projection = matter::row_json("asset")?;
    let sql = format!("SELECT {projection} AS row FROM asset r WHERE id=$1");
    let snapshot: Option<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    let mut planned = snapshot.clone();
    for (name, value) in changes
        .as_object()
        .ok_or(AssetError::WriteReturnedNothing)?
    {
        planned[name] = value.clone();
    }
    lock_keys(&mut tx, Some(&snapshot), &planned).await?;
    let sql = format!("SELECT {projection} AS row FROM asset r WHERE id=$1 FOR UPDATE");
    let prior: Option<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(prior) = prior else { return Ok(None) };
    if notation_parents(&prior) != notation_parents(&snapshot) {
        return Err(AssetError::ConcurrentAssociation);
    }
    let mut next = prior.clone();
    for (name, value) in changes
        .as_object()
        .ok_or(AssetError::WriteReturnedNothing)?
    {
        if matches!(name.as_str(), "id" | "project_id" | "inserted_at") {
            return Err(matter::MatterAuthorityError::Unregistered {
                table: "asset",
                column: name.clone(),
            }
            .into());
        }
        next[name] = value.clone();
    }
    for name in clear {
        if !matter::json_column("asset", name)? {
            return Err(matter::MatterAuthorityError::Unregistered {
                table: "asset",
                column: (*name).into(),
            }
            .into());
        }
        next.as_object_mut()
            .ok_or(AssetError::WriteReturnedNothing)?
            .remove(*name);
    }
    next["updated_at"] = json!(Utc::now());

    let stored: Value = matter::upsert_on(&mut *tx, "asset", next).await?;
    let result = decode_value(stored)?;
    tx.commit().await?;
    Ok(Some(result))
}

/// Insert new metadata; callers choose a new identity and never replace an old row.
pub(crate) async fn insert_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    row: Value,
) -> Result<Asset, AssetError> {
    lock_keys(tx, None, &row).await?;
    let stored: Option<Value> = matter::insert_on(&mut **tx, "asset", row).await?;
    let stored =
        stored.ok_or_else(|| matter::MatterAuthorityError::Conflict("asset_pkey".into()))?;
    decode_value(stored)
}

pub(crate) async fn ingest_content(
    pg: &PostgresDb,
    storage: &Arc<dyn StorageService>,
    bytes: &[u8],
    content_type: &str,
) -> Result<Uuid, AssetError> {
    let sha = crate::assets::sha256_hex(bytes);
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(1095979845,hashtext($1))")
        .bind(&sha)
        .execute(&mut *tx)
        .await?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM asset WHERE sha256_hex=$1 AND filename IS NULL ORDER BY id LIMIT 1",
    )
    .bind(&sha)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = existing {
        tx.commit().await?;
        return Ok(id);
    }
    let key = format!("blobs/{sha}");
    storage.put(&key, bytes, content_type).await?;
    let now = Utc::now();
    let row=insert_on(&mut tx,json!({"id":Uuid::now_v7(),"storage_key":key,"content_type":content_type,"byte_size":i64::try_from(bytes.len()).unwrap_or(i64::MAX),"sha256_hex":sha,"visibility":crate::documents::visibility::INTERNAL,"notarization":[],"inserted_at":now,"updated_at":now})).await?;
    tx.commit().await?;
    Ok(row.id)
}

pub(crate) async fn count(
    pg: &PostgresDb,
    suffix: &'static str,
    filter: Value,
) -> Result<usize, AssetError> {
    require_ready(pg).await?;
    let sql = format!("SELECT count(*) FROM asset r WHERE $1::jsonb IS NOT NULL {suffix}");
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(filter)
        .fetch_one(pg.pool())
        .await?;
    usize::try_from(count).map_err(|_| AssetError::WriteReturnedNothing)
}

pub(crate) async fn delete(pg: &PostgresDb, id: Uuid) -> Result<Option<Asset>, AssetError> {
    let mut tx = pg.pool().begin().await?;
    let projection = matter::row_json("asset")?;
    let sql = format!("SELECT {projection} AS row FROM asset r WHERE id=$1");
    let snapshot: Option<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    lock_keys(&mut tx, Some(&snapshot), &snapshot).await?;
    let sql = format!("SELECT {projection} AS row FROM asset r WHERE id=$1 FOR UPDATE");
    let row: Option<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else { return Ok(None) };
    if notation_parents(&row) != notation_parents(&snapshot) {
        return Err(AssetError::ConcurrentAssociation);
    }
    let asset = decode_value(row)?;
    sqlx::query("DELETE FROM asset WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(asset))
}

pub(crate) async fn restore(
    pg: &PostgresDb,
    archived: &Asset,
) -> Result<crate::assets::RowRestore, AssetError> {
    restore_inner(pg, archived, None).await
}

pub(crate) async fn restore_history(
    parents: &crate::project_archives::restore_parents::RestoreParents<'_>,
    archived: &Asset,
) -> Result<crate::assets::RowRestore, AssetError> {
    if !parents.verifies_asset(archived) {
        return Err(AssetError::WriteReturnedNothing);
    }
    restore_inner(parents.database(), archived, Some(parents)).await
}

async fn restore_inner(
    pg: &PostgresDb,
    archived: &Asset,
    parents: Option<&crate::project_archives::restore_parents::RestoreParents<'_>>,
) -> Result<crate::assets::RowRestore, AssetError> {
    use chrono::SubsecRound;
    let mut expected = archived.clone();
    expected.inserted_at = expected.inserted_at.trunc_subsecs(6);
    expected.updated_at = expected.updated_at.trunc_subsecs(6);
    for field in [
        &mut expected.deleted_at,
        &mut expected.purge_after,
        &mut expected.legal_hold_at,
        &mut expected.purged_at,
    ] {
        *field = field.map(|at| at.trunc_subsecs(6));
    }
    let mut row = serde_json::to_value(&expected).map_err(matter::MatterAuthorityError::from)?;
    for field in ["signing", "evidence"] {
        if row.get(field).is_some_and(Value::is_null) {
            row.as_object_mut()
                .ok_or(AssetError::WriteReturnedNothing)?
                .remove(field);
        }
    }
    let mut tx = pg.pool().begin().await?;
    let sql = format!(
        "SELECT {} AS row FROM asset r WHERE id=$1 FOR UPDATE",
        matter::row_json("asset")?
    );
    let existing: Option<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(expected.id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(existing) = existing {
        let json_equal = ["metadata", "derived_from"]
            .iter()
            .all(|field| existing.get(*field) == row.get(*field));
        let identical = decode_value(existing)? == expected && json_equal;
        tx.commit().await?;
        return Ok(if identical {
            crate::assets::RowRestore::Identical
        } else {
            crate::assets::RowRestore::Conflict
        });
    }
    if let Some(parents) = parents {
        for key in [
            Some(archived.storage_key.as_str()),
            archived.secondary_storage_key.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(id) = notation_parent(key) {
                if !parents.permits_note(id) {
                    return Err(AssetError::WriteReturnedNothing);
                }
            }
        }
    } else {
        lock_keys(&mut tx, None, &row).await?;
    }

    let inserted: Option<Value> = matter::insert_on(&mut *tx, "asset", row).await?;
    tx.commit().await?;
    if inserted.is_some() {
        Ok(crate::assets::RowRestore::Created)
    } else {
        restore_existing(pg, &expected).await
    }
}

async fn restore_existing(
    pg: &PostgresDb,
    expected: &Asset,
) -> Result<crate::assets::RowRestore, AssetError> {
    let rows = rows(pg, "AND r.id=($1->>'id')::uuid", json!({"id":expected.id})).await?;
    Ok(if rows.first() == Some(expected) {
        crate::assets::RowRestore::Identical
    } else {
        crate::assets::RowRestore::Conflict
    })
}

pub(crate) fn document_row(
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: Option<&crate::documents::MailProvenance<'_>>,
    document: &crate::documents::IngestedDocument,
) -> Value {
    let now = Utc::now();
    let mut row = json!({
        "id":document.asset_id,"project_id":args.project_id,"storage_key":document.storage_key,
        "secondary_storage_key":args.secondary_storage_key,"content_type":args.content_type,
        "byte_size":document.byte_size,"sha256_hex":document.sha256_hex,"filename":args.filename,
        "kind":args.kind,"source":args.source,"received_at":now.to_rfc3339(),"description":args.description,
        "visibility":args.visibility,"slug":identity.slug,"published_at":identity.published_at,
        "source_message_id":provenance.map(|p|p.message_id),"source_sender":provenance.map(|p|p.sender),
        "source_received_at":provenance.map(|p|p.received_at),"source_subject":provenance.map(|p|p.subject),
        "metadata":identity.metadata,"derived_from":identity.derived_from,"transcript_quality":identity.transcript_quality,
        "signing":identity.signing,"notarization":identity.notarization,"evidence":identity.evidence,
        "folder":identity.folder,"inserted_at":now,"updated_at":now
    });
    for (field, missing) in [
        ("metadata", identity.metadata.is_none()),
        ("derived_from", identity.derived_from.is_none()),
        ("signing", identity.signing.is_none()),
        ("evidence", identity.evidence.is_none()),
    ] {
        if missing {
            if let Value::Object(fields) = &mut row {
                fields.remove(field);
            }
        }
    }
    row
}

pub(crate) async fn project_code(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<Option<String>, AssetError> {
    sqlx::query_scalar("SELECT code FROM project WHERE id=$1")
        .bind(project)
        .fetch_optional(pg.pool())
        .await
        .map_err(AssetError::from)
}

pub(crate) async fn ingest_document(
    pg: &PostgresDb,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: Option<&crate::documents::MailProvenance<'_>>,
    id: Option<Uuid>,
    bytes: &[u8],
) -> Result<crate::documents::IngestedDocument, crate::documents::IngestError> {
    let code = project_code(pg, args.project_id).await?;
    let code = code.ok_or(crate::projects::ProjectStoreError::NoSuchProject(
        args.project_id,
    ))?;
    let sha = crate::assets::sha256_hex(bytes);
    let key = format!("{}/{sha}", cloud::workspace::documents_prefix(&code));
    let reused = count(
        pg,
        "AND r.project_id=($1->>'project')::uuid AND r.sha256_hex=$1->>'sha'",
        json!({"project":args.project_id,"sha":sha}),
    )
    .await?
        > 0;
    if !reused {
        storage.put(&key, bytes, args.content_type).await?;
    }
    let document = crate::documents::IngestedDocument {
        asset_id: id.unwrap_or_else(Uuid::now_v7),
        storage_key: key,
        sha256_hex: sha,
        byte_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        reused,
    };
    let mut tx = pg.pool().begin().await.map_err(AssetError::from)?;
    insert_on(&mut tx, document_row(args, identity, provenance, &document)).await?;
    tx.commit().await.map_err(AssetError::from)?;
    Ok(document)
}

fn filing_matches_head(
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    head: &Asset,
    sha: &str,
) -> bool {
    use crate::assets::verified_onboarding_envelope;
    let evidence = verified_onboarding_envelope(identity.metadata.as_ref());
    head.deleted_at.is_none()
        && head.sha256_hex == sha
        && identity
            .signing
            .is_none_or(|signing| head.signing == Some(signing))
        && crate::documents::matches_provider_filing(args, identity, head)
        && (args.kind != "onboarding"
            || evidence.is_none()
            || verified_onboarding_envelope(head.metadata.as_ref()) == evidence)
}

pub(crate) async fn file_revision(
    pg: &PostgresDb,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: Option<&crate::documents::MailProvenance<'_>>,
    expected: Option<&crate::assets::ExpectedHead>,
    bytes: &[u8],
) -> Result<crate::assets::Filed, crate::assets::RevisionError> {
    use crate::assets::{Filed, RevisionError};
    require_ready(pg).await?;
    if !rules::kind::Kind::parse(args.kind)
        .is_some_and(|kind| kind.valid_for(rules::kind::Lane::Asset))
    {
        return Err(RevisionError::KindNotFilable(args.kind.into()));
    }
    crate::document_pointers::MatterFolder::parse_checked_folder(identity.folder)
        .map_err(crate::documents::IngestError::from)?;
    let mut identity = identity.clone();
    if identity.slug.is_none() && args.kind == "onboarding" {
        identity.slug = Some(args.filename);
    }
    let Some(slug) = identity.slug else {
        return Ok(Filed::Revision(
            ingest_document(pg, storage, args, &identity, provenance, None, bytes).await?,
        ));
    };
    let sha = crate::assets::sha256_hex(bytes);
    let mut tx = pg.pool().begin().await.map_err(AssetError::from)?;
    let code: Option<String> =
        sqlx::query_scalar("SELECT code FROM project WHERE id=$1 FOR UPDATE")
            .bind(args.project_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AssetError::from)?;
    let code = code.ok_or_else(|| {
        crate::documents::IngestError::Project(crate::projects::ProjectStoreError::NoSuchProject(
            args.project_id,
        ))
    })?;
    let sql = format!(
        "SELECT {} AS row FROM asset r WHERE project_id=$1 AND slug=$2 ORDER BY id DESC",
        matter::row_json("asset").map_err(AssetError::from)?
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(args.project_id)
        .bind(slug)
        .fetch_all(&mut *tx)
        .await
        .map_err(AssetError::from)?;
    let chain = rows
        .iter()
        .map(|row| decode_value(row.try_get("row").map_err(AssetError::from)?))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(head) = chain.first() {
        if let Some(kind) = head.kind.as_deref().filter(|kind| *kind != args.kind) {
            return Err(RevisionError::KindChanged {
                slug: slug.into(),
                existing: kind.into(),
                attempted: args.kind.into(),
            });
        }
        crate::assets::preserve_drive_metadata(&mut identity, head)?;
        if filing_matches_head(args, &identity, head, &sha) {
            return Ok(Filed::Unchanged { asset_id: head.id });
        }
    }
    if let Some(expected) = expected {
        let current = chain
            .iter()
            .find(|head| head.deleted_at.is_none())
            .map(|head| crate::assets::head_triple(head, chain.len()));
        if current.as_ref() != Some(expected) {
            return Err(RevisionError::HeadConflict {
                slug: slug.into(),
                current,
            });
        }
    }
    let key = format!("{}/{sha}", cloud::workspace::documents_prefix(&code));
    storage
        .put(&key, bytes, args.content_type)
        .await
        .map_err(crate::documents::IngestError::from)?;
    let document = crate::documents::IngestedDocument {
        asset_id: Uuid::now_v7(),
        storage_key: key,
        sha256_hex: sha,
        byte_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        reused: false,
    };
    insert_on(
        &mut tx,
        document_row(args, &identity, provenance, &document),
    )
    .await?;
    // This changes no protection/key fact; keep the existing parent association.
    sqlx::query("UPDATE asset SET visibility='internal',updated_at=now() WHERE project_id=$1 AND slug=$2 AND id<>$3 AND visibility<>'internal'")
        .bind(args.project_id).bind(slug).bind(document.asset_id).execute(&mut *tx).await.map_err(AssetError::from)?;
    tx.commit().await.map_err(AssetError::from)?;
    Ok(Filed::Revision(document))
}

async fn require_ready(pg: &PostgresDb) -> Result<(), AssetError> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(matter::MatterAuthorityError::from)?;
    Ok(())
}
