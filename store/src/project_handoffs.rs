//! Legal-files-only handoff bundles.
//!
//! A handoff gives one recipient the Project's legal files and nothing else:
//! the current revision of every live document the recipient's own matter
//! lens admits. Source and history (a `closed_repository` archive), Notation
//! internals, credentials, and the recovery archive's restore metadata never
//! enter it. [`prepare`] reads every file's bytes from Navigator storage — the
//! active copy, or the verified archived copy once the active one is gone —
//! checks each against its recorded size and SHA-256, and only then records an
//! immutable manifest. A missing or damaged file fails the handoff rather than
//! producing a bundle that looks complete. [`bundle`] rebuilds the ZIP from
//! that manifest, re-verifying every byte, so the same handoff is always the
//! same files.

use std::collections::BTreeSet;
use std::io::{Cursor, Write as _};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::{StorageError, StorageService};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::access::ProjectLens;
use crate::assets::Asset;
use crate::persons::Role;
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

/// The manifest format this build writes and reads.
pub const FORMAT: u32 = 1;
/// The most files one handoff may carry.
pub const MAX_FILES: usize = 2_000;
/// The most bytes one handoff may carry.
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// The manifest's own path inside the bundle.
pub const MANIFEST_PATH: &str = "MANIFEST.json";

/// One file in a handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffFile {
    /// The entry name inside the bundle.
    pub path: String,
    pub asset_id: Uuid,
    pub slug: Option<String>,
    pub kind: Option<String>,
    pub sha256: String,
    pub size_bytes: i64,
}

/// The immutable inventory one recipient receives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffManifest {
    pub format: u32,
    pub handoff_id: Uuid,
    pub project_id: Uuid,
    pub recipient_person_id: Uuid,
    pub lens: String,
    pub created_at: DateTime<Utc>,
    pub files: Vec<HandoffFile>,
}

/// One recorded handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Handoff {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Uuid,
    pub recipient_person_id: Uuid,
    pub lens: String,
    pub required: bool,
    pub manifest_sha256: String,
    pub file_count: i32,
    pub total_bytes: i64,
    pub created_by_person_id: Uuid,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub manifest: HandoffManifest,
}

/// Why a handoff was refused. Messages carry ids, never document content.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("project not found")]
    ProjectNotFound,
    #[error("the project belongs to no Firm")]
    NoFirm,
    #[error("the recipient does not participate in this Project")]
    RecipientNotParticipant,
    #[error("handoff not found")]
    NotFound,
    #[error("revision {0} is missing from Navigator storage")]
    Missing(Uuid),
    #[error("revision {0} does not match its recorded size and digest")]
    Corrupt(Uuid),
    #[error("the handoff exceeds {MAX_FILES} files or {MAX_BYTES} bytes")]
    TooLarge,
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("database: {0}")]
    Database(String),
    #[error("store: {0}")]
    Store(String),
}

impl From<sqlx::Error> for HandoffError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

fn store_error(error: impl std::fmt::Display) -> HandoffError {
    HandoffError::Store(error.to_string())
}

fn lens_name(lens: ProjectLens) -> &'static str {
    match lens {
        ProjectLens::Client => "client",
        ProjectLens::Lawyer => "lawyer",
    }
}

/// The current revision of every live legal document `lens` admits, oldest
/// document first. A `closed_repository` archive is source and history, not a
/// legal file, and never ships.
///
/// # Errors
/// A store failure.
pub async fn inventory(
    surreal: &SurrealDb,
    project_id: Uuid,
    lens: ProjectLens,
) -> Result<Vec<Asset>, HandoffError> {
    let mut rows = crate::assets::for_project(surreal, project_id)
        .await
        .map_err(store_error)?;
    rows.retain(|row| {
        row.kind.as_deref() != Some(rules::kind::Kind::ClosedRepository.as_str())
            && row.deleted_at.is_none()
            && (lens == ProjectLens::Lawyer
                || row.visibility == crate::documents::visibility::CLIENT)
    });
    // Newest first: the first row of each chain is its current revision for
    // this lens.
    let mut seen = BTreeSet::new();
    let mut current: Vec<Asset> = rows
        .into_iter()
        .filter(|row| seen.insert(crate::document_retention::chain_key(row)))
        .collect();
    current.reverse();
    Ok(current)
}

/// Read one revision's bytes: the active copy, or the verified archived copy
/// when the active one is gone. Either must match the recorded digest.
async fn read_verified(
    documents: &Arc<dyn StorageService>,
    archive: Option<&Arc<dyn StorageService>>,
    firm_id: Uuid,
    asset: &Asset,
) -> Result<Vec<u8>, HandoffError> {
    let bytes = match documents.get(&asset.storage_key).await {
        Ok(object) => object.bytes,
        Err(StorageError::NotFound(_)) => {
            let Some(archive) = archive else {
                return Err(HandoffError::Missing(asset.id));
            };
            let project_id = asset.project_id.ok_or(HandoffError::Missing(asset.id))?;
            match archive
                .get(&crate::project_archives::object_key(
                    firm_id,
                    project_id,
                    &asset.sha256_hex,
                ))
                .await
            {
                Ok(object) => object.bytes,
                Err(StorageError::NotFound(_)) => return Err(HandoffError::Missing(asset.id)),
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    if i64::try_from(bytes.len()).ok() != Some(asset.byte_size)
        || crate::assets::sha256_hex(&bytes) != asset.sha256_hex
    {
        return Err(HandoffError::Corrupt(asset.id));
    }
    Ok(bytes)
}

fn entry_name(asset: &Asset, used: &mut BTreeSet<String>) -> String {
    let raw = asset
        .filename
        .as_deref()
        .and_then(|name| name.rsplit(['/', '\\']).next())
        .filter(|name| !name.trim().is_empty() && *name != "." && *name != "..")
        .unwrap_or("document");
    let clean: String = raw
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let mut name = clean.clone();
    let mut n = 2;
    while !used.insert(name.to_ascii_lowercase()) || name == MANIFEST_PATH {
        let (stem, ext) = clean
            .rsplit_once('.')
            .map_or((clean.as_str(), ""), |(s, e)| (s, e));
        name = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        n += 1;
    }
    name
}

/// Prepare a handoff for one participant of the Project.
///
/// The actor must reach the Project at lawyer tier; the recipient's own matter
/// lens decides what ships. Every byte is read and verified before the
/// manifest is recorded.
///
/// # Errors
/// [`HandoffError`] for an out-of-scope Project, a non-participant recipient,
/// a missing or damaged file, an oversize handoff, or a store failure.
#[allow(clippy::too_many_arguments)]
pub async fn prepare(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    documents: &Arc<dyn StorageService>,
    archive: Option<&Arc<dyn StorageService>>,
    project_id: Uuid,
    actor: (Uuid, Role),
    recipient_person_id: Uuid,
    required: bool,
    now: DateTime<Utc>,
) -> Result<Handoff, HandoffError> {
    let (actor_id, actor_role) = actor;
    if !crate::access::can_see_project_as_lawyer(surreal, Some(actor_id), actor_role, project_id)
        .await
        .map_err(store_error)?
    {
        return Err(HandoffError::ProjectNotFound);
    }
    let project = crate::projects::find_by_id(surreal, project_id)
        .await
        .map_err(store_error)?
        .ok_or(HandoffError::ProjectNotFound)?;
    let firm_id = project.firm_id.ok_or(HandoffError::NoFirm)?;
    let recipient = crate::persons::find_by_id(surreal, recipient_person_id)
        .await
        .map_err(store_error)?
        .ok_or(HandoffError::RecipientNotParticipant)?;
    let lens = crate::access::matter_lens(surreal, Some(recipient.id), recipient.role, project_id)
        .await
        .map_err(store_error)?
        .ok_or(HandoffError::RecipientNotParticipant)?;
    let rows = inventory(surreal, project_id, lens).await?;
    if rows.len() > MAX_FILES {
        return Err(HandoffError::TooLarge);
    }
    let mut total: u64 = 0;
    let mut used = BTreeSet::new();
    let mut files = Vec::with_capacity(rows.len());
    for row in &rows {
        read_verified(documents, archive, firm_id, row).await?;
        total = total.saturating_add(u64::try_from(row.byte_size).unwrap_or(u64::MAX));
        if total > MAX_BYTES {
            return Err(HandoffError::TooLarge);
        }
        files.push(HandoffFile {
            path: entry_name(row, &mut used),
            asset_id: row.id,
            slug: row.slug.clone(),
            kind: row.kind.clone(),
            sha256: row.sha256_hex.clone(),
            size_bytes: row.byte_size,
        });
    }
    let id = Uuid::now_v7();
    let manifest = HandoffManifest {
        format: FORMAT,
        handoff_id: id,
        project_id,
        recipient_person_id,
        lens: lens_name(lens).to_string(),
        created_at: now,
        files,
    };
    let manifest_json = serde_json::to_value(&manifest).map_err(store_error)?;
    let manifest_sha256 =
        crate::assets::sha256_hex(&serde_json::to_vec(&manifest).map_err(store_error)?);
    sqlx::query(
        "INSERT INTO project_handoff \
         (id, firm_id, project_id, recipient_person_id, lens, required, manifest, \
          manifest_sha256, file_count, total_bytes, created_by_person_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(id)
    .bind(firm_id)
    .bind(project_id)
    .bind(recipient_person_id)
    .bind(&manifest.lens)
    .bind(required)
    .bind(&manifest_json)
    .bind(&manifest_sha256)
    .bind(i32::try_from(manifest.files.len()).unwrap_or(i32::MAX))
    .bind(i64::try_from(total).unwrap_or(i64::MAX))
    .bind(actor_id)
    .bind(now)
    .execute(pg.pool())
    .await?;
    find(pg, project_id, id).await
}

const COLUMNS: &str = "id, firm_id, project_id, recipient_person_id, lens, required, manifest, \
     manifest_sha256, file_count, total_bytes, created_by_person_id, created_at";

fn from_row(row: &sqlx::postgres::PgRow) -> Result<Handoff, HandoffError> {
    let manifest: serde_json::Value = row.try_get("manifest")?;
    Ok(Handoff {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        project_id: row.try_get("project_id")?,
        recipient_person_id: row.try_get("recipient_person_id")?,
        lens: row.try_get("lens")?,
        required: row.try_get("required")?,
        manifest_sha256: row.try_get("manifest_sha256")?,
        file_count: row.try_get("file_count")?,
        total_bytes: row.try_get("total_bytes")?,
        created_by_person_id: row.try_get("created_by_person_id")?,
        created_at: row.try_get("created_at")?,
        manifest: serde_json::from_value(manifest).map_err(store_error)?,
    })
}

/// One handoff of `project_id`.
///
/// # Errors
/// [`HandoffError::NotFound`] for a missing or foreign handoff.
pub async fn find(pg: &PostgresDb, project_id: Uuid, id: Uuid) -> Result<Handoff, HandoffError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_handoff WHERE id = $1 AND project_id = $2"
    )))
    .bind(id)
    .bind(project_id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(HandoffError::NotFound)?;
    from_row(&row)
}

/// Every handoff of `project_id`, newest first.
///
/// # Errors
/// A database error.
pub async fn for_project(pg: &PostgresDb, project_id: Uuid) -> Result<Vec<Handoff>, HandoffError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_handoff WHERE project_id = $1 ORDER BY created_at DESC"
    )))
    .bind(project_id)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(from_row).collect()
}

/// Rebuild a handoff's ZIP from its manifest: `MANIFEST.json` first, then every
/// file, each re-read and re-verified against the manifest.
///
/// # Errors
/// [`HandoffError`] when a file is now missing or damaged, or a store failure.
pub async fn bundle(
    surreal: &SurrealDb,
    documents: &Arc<dyn StorageService>,
    archive: Option<&Arc<dyn StorageService>>,
    handoff: &Handoff,
) -> Result<Vec<u8>, HandoffError> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    let manifest = serde_json::to_vec_pretty(&handoff.manifest).map_err(store_error)?;
    let write = |writer: &mut zip::ZipWriter<Cursor<Vec<u8>>>, path: &str, bytes: &[u8]| {
        writer
            .start_file(path, options)
            .and_then(|()| writer.write_all(bytes).map_err(Into::into))
            .map_err(|error| HandoffError::Store(error.to_string()))
    };
    write(&mut writer, MANIFEST_PATH, &manifest)?;
    for file in &handoff.manifest.files {
        let asset = crate::assets::find_by_id(surreal, file.asset_id)
            .await
            .map_err(store_error)?
            .filter(|asset| {
                asset.project_id == Some(handoff.project_id)
                    && asset.sha256_hex == file.sha256
                    && asset.byte_size == file.size_bytes
            })
            .ok_or(HandoffError::Missing(file.asset_id))?;
        let bytes = read_verified(documents, archive, handoff.firm_id, &asset).await?;
        write(&mut writer, &file.path, &bytes)?;
    }
    writer
        .finish()
        .map(Cursor::into_inner)
        .map_err(|error| HandoffError::Store(error.to_string()))
}
