//! Initialize a Project's source repository from a validated headless source
//! archive, durably.
//!
//! [`request`] validates the uploaded ZIP with
//! [`rules::source_archive::canonicalize`] before anything is written: an
//! invalid archive changes nothing. A valid one is stored as its canonical ZIP
//! in `project_source_import`, bound to its tree digest, the Project, its Firm,
//! and an operation key. That stored archive is the recovery source: [`process`]
//! re-reads and re-validates it, creates or adopts the Firm's repository, and
//! writes the tree as the first commit with the digest in a trailer, so a retry
//! after a worker loss or an unknown forge outcome finds the applied commit
//! instead of writing a second one. Only after the forge reports that commit at
//! the head does one statement record the repository URL, its provisioning
//! time, and `git_initialized_at`; then the stored archive is cleared.
//!
//! An existing repository with other source is never replaced, a Project whose
//! source is already initialized refuses a new import, and at most one import
//! per Project is pending. Archive contents never enter logs, errors, or the
//! durable-execution journal: errors name paths and rules only.

use chrono::{DateTime, Utc};
use cloud::forge::{ForgeError, ForgeService};
use rules::source_archive::{self, CanonicalTree, SourceArchiveError};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::persons::Role;
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

const OPERATION_KEY_MAX: usize = 200;
const COMMIT_MESSAGE: &str = "Initialize Project source from a validated headless archive";

/// Where an import stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportState {
    Pending,
    Committed,
    Refused,
}

/// One import row, without its archive bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceImport {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Uuid,
    pub operation_key: String,
    pub tree_sha256: String,
    pub file_count: i32,
    pub state: ImportState,
    pub requested_by_person_id: Uuid,
    pub requested_at: DateTime<Utc>,
    pub repository_url: Option<String>,
    pub commit_sha: Option<String>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Why an import was refused or stopped.
#[derive(Debug, thiserror::Error)]
pub enum SourceImportError {
    #[error("invalid source archive: {0}")]
    Archive(#[from] SourceArchiveError),
    #[error("project not found")]
    ProjectNotFound,
    #[error("the project belongs to no Firm")]
    NoFirm,
    #[error("operation keys are 1 to 200 visible ASCII characters")]
    InvalidOperationKey,
    #[error("the operation key already names a different import")]
    OperationKeyConflict,
    #[error("another import of this Project is pending")]
    ImportPending,
    #[error("the Project's source is already initialized")]
    AlreadyInitialized,
    #[error("the Project records a different repository than the one initialized")]
    PointerConflict,
    #[error("import not found")]
    NotFound,
    #[error("forge: {0}")]
    Forge(#[from] ForgeError),
    #[error("database: {0}")]
    Database(String),
    #[error("store: {0}")]
    Store(String),
}

impl From<sqlx::Error> for SourceImportError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(database) = &error {
            if database.constraint() == Some("project_source_import_one_pending") {
                return Self::ImportPending;
            }
        }
        Self::Database(error.to_string())
    }
}

fn store_error(error: impl std::fmt::Display) -> SourceImportError {
    SourceImportError::Store(error.to_string())
}

const COLUMNS: &str = "id, firm_id, project_id, operation_key, tree_sha256, file_count, state, \
     requested_by_person_id, requested_at, repository_url, commit_sha, attempts, last_error, \
     finished_at";

fn from_row(row: &sqlx::postgres::PgRow) -> Result<SourceImport, SourceImportError> {
    Ok(SourceImport {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        project_id: row.try_get("project_id")?,
        operation_key: row.try_get("operation_key")?,
        tree_sha256: row.try_get("tree_sha256")?,
        file_count: row.try_get("file_count")?,
        state: match row.try_get::<&str, _>("state")? {
            "pending" => ImportState::Pending,
            "committed" => ImportState::Committed,
            _ => ImportState::Refused,
        },
        requested_by_person_id: row.try_get("requested_by_person_id")?,
        requested_at: row.try_get("requested_at")?,
        repository_url: row.try_get("repository_url")?,
        commit_sha: row.try_get("commit_sha")?,
        attempts: row.try_get("attempts")?,
        last_error: row.try_get("last_error")?,
        finished_at: row.try_get("finished_at")?,
    })
}

/// One import, if it belongs to `project_id`.
///
/// # Errors
/// [`SourceImportError::NotFound`] for a missing or foreign import.
pub async fn find(
    pg: &PostgresDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<SourceImport, SourceImportError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_source_import WHERE id = $1 AND project_id = $2"
    )))
    .bind(id)
    .bind(project_id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(SourceImportError::NotFound)?;
    from_row(&row)
}

/// One import by id, for the durable worker acting on its recorded Project.
///
/// # Errors
/// [`SourceImportError::NotFound`] or a database error.
pub async fn get(pg: &PostgresDb, id: Uuid) -> Result<SourceImport, SourceImportError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_source_import WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(SourceImportError::NotFound)?;
    from_row(&row)
}

/// Pending imports, oldest first: the durable queue the import schedule
/// drains.
///
/// # Errors
/// A database error.
pub async fn pending(pg: &PostgresDb, limit: i64) -> Result<Vec<SourceImport>, SourceImportError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_source_import WHERE state = 'pending' \
         ORDER BY requested_at ASC, id ASC LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(from_row).collect()
}

/// Validate `archive` and record a durable import request.
///
/// The actor must reach the Project at lawyer tier. An invalid archive,
/// a foreign Project, or a Project whose source is already initialized writes
/// nothing. The same operation key with the same Project and tree returns the
/// original import; with anything else it is a conflict.
///
/// # Errors
/// [`SourceImportError`] naming the refusal.
pub async fn request(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    actor: (Uuid, Role),
    operation_key: &str,
    archive: &[u8],
    now: DateTime<Utc>,
) -> Result<SourceImport, SourceImportError> {
    if operation_key.is_empty()
        || operation_key.len() > OPERATION_KEY_MAX
        || !operation_key.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(SourceImportError::InvalidOperationKey);
    }
    let tree = source_archive::canonicalize(archive)?;
    let (person_id, role) = actor;
    if !crate::access::can_see_project_as_lawyer(surreal, Some(person_id), role, project_id)
        .await
        .map_err(store_error)?
    {
        return Err(SourceImportError::ProjectNotFound);
    }
    let project = crate::projects::find_by_id(surreal, project_id)
        .await
        .map_err(store_error)?
        .ok_or(SourceImportError::ProjectNotFound)?;
    let firm_id = project.firm_id.ok_or(SourceImportError::NoFirm)?;
    let digest = tree.digest();
    if let Some(existing) = by_operation_key(pg, operation_key).await? {
        return if existing.project_id == project_id && existing.tree_sha256 == digest {
            Ok(existing)
        } else {
            Err(SourceImportError::OperationKeyConflict)
        };
    }
    if project.git_initialized_at.is_some() {
        return Err(SourceImportError::AlreadyInitialized);
    }
    let canonical = source_archive::write_canonical_zip(&tree)?;
    sqlx::query(
        "INSERT INTO project_source_import \
         (id, firm_id, project_id, operation_key, tree_sha256, file_count, canonical_zip, state, \
          requested_by_person_id, requested_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', $8, $9) \
         ON CONFLICT (operation_key) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(firm_id)
    .bind(project_id)
    .bind(operation_key)
    .bind(&digest)
    .bind(i32::try_from(tree.files.len()).unwrap_or(i32::MAX))
    .bind(&canonical)
    .bind(person_id)
    .bind(now)
    .execute(pg.pool())
    .await?;
    let stored = by_operation_key(pg, operation_key)
        .await?
        .ok_or(SourceImportError::NotFound)?;
    if stored.project_id != project_id || stored.tree_sha256 != digest {
        return Err(SourceImportError::OperationKeyConflict);
    }
    Ok(stored)
}

async fn by_operation_key(
    pg: &PostgresDb,
    operation_key: &str,
) -> Result<Option<SourceImport>, SourceImportError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_source_import WHERE operation_key = $1"
    )))
    .bind(operation_key)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(from_row).transpose()
}

/// Apply one pending import through `forge`, which must be confined to the
/// Project's Firm. Safe to call again after any failure: the stored archive is
/// re-validated, the first commit is found by its tree digest, and the
/// Project row changes only after the forge reports that commit at the head.
///
/// # Errors
/// [`SourceImportError`]; the import stays pending with its error recorded,
/// except a repository holding other source, which refuses it for good.
pub async fn process(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    forge: &dyn ForgeService,
    import_id: Uuid,
    now: DateTime<Utc>,
) -> Result<SourceImport, SourceImportError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE project_source_import SET attempts = attempts + 1 \
         WHERE id = $1 AND state = 'pending' RETURNING {COLUMNS}, canonical_zip"
    )))
    .bind(import_id)
    .fetch_optional(pg.pool())
    .await?;
    let Some(row) = row else {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM project_source_import WHERE id = $1"
        )))
        .bind(import_id)
        .fetch_optional(pg.pool())
        .await?
        .ok_or(SourceImportError::NotFound)?;
        return from_row(&row);
    };
    let import = from_row(&row)?;
    let archive: Vec<u8> = row.try_get("canonical_zip")?;
    match apply(surreal, forge, &import, &archive, now).await {
        Ok((repository_url, commit_sha)) => {
            let row = sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE project_source_import SET state = 'committed', canonical_zip = NULL, \
                 repository_url = $2, commit_sha = $3, finished_at = $4, last_error = NULL \
                 WHERE id = $1 AND state = 'pending' RETURNING {COLUMNS}"
            )))
            .bind(import_id)
            .bind(&repository_url)
            .bind(&commit_sha)
            .bind(now)
            .fetch_optional(pg.pool())
            .await?;
            match row {
                Some(row) => from_row(&row),
                None => find(pg, import.project_id, import_id).await,
            }
        }
        Err(error) => {
            let refused = matches!(
                error,
                SourceImportError::Forge(
                    ForgeError::RepositoryNotEmpty { .. }
                        | ForgeError::ForeignRepository { .. }
                        | ForgeError::Unsupported(_)
                ) | SourceImportError::Archive(_)
                    | SourceImportError::PointerConflict
            );
            sqlx::query(
                "UPDATE project_source_import SET last_error = $2, \
                 state = CASE WHEN $3 THEN 'refused' ELSE state END, \
                 canonical_zip = CASE WHEN $3 THEN NULL ELSE canonical_zip END, \
                 finished_at = CASE WHEN $3 THEN $4 ELSE finished_at END \
                 WHERE id = $1 AND state = 'pending'",
            )
            .bind(import_id)
            .bind(error.to_string())
            .bind(refused)
            .bind(now)
            .execute(pg.pool())
            .await?;
            Err(error)
        }
    }
}

async fn apply(
    surreal: &SurrealDb,
    forge: &dyn ForgeService,
    import: &SourceImport,
    archive: &[u8],
    now: DateTime<Utc>,
) -> Result<(String, String), SourceImportError> {
    let tree: CanonicalTree = source_archive::canonicalize(archive)?;
    if tree.digest() != import.tree_sha256 {
        return Err(SourceImportError::Archive(SourceArchiveError::Unreadable));
    }
    let project = crate::projects::find_by_id(surreal, import.project_id)
        .await
        .map_err(store_error)?
        .ok_or(SourceImportError::ProjectNotFound)?;
    if project.firm_id != Some(import.firm_id) {
        return Err(SourceImportError::ProjectNotFound);
    }
    let repository = forge.ensure_repository(&project.code).await?;
    if project
        .repository_url
        .as_deref()
        .is_some_and(|recorded| recorded != repository.url)
    {
        return Err(SourceImportError::PointerConflict);
    }
    let commit = forge
        .initialize_source(
            &project.code,
            &tree.files,
            &import.tree_sha256,
            COMMIT_MESSAGE,
        )
        .await?;
    if forge.head_commit_sha(&project.code).await?.as_deref() != Some(commit.as_str()) {
        return Err(SourceImportError::Forge(ForgeError::Api {
            action: "verifying the initial commit",
            status: 409,
        }));
    }
    let stored = crate::projects::record_source_initialized(
        surreal,
        import.project_id,
        &repository.url,
        &now.to_rfc3339(),
    )
    .await
    .map_err(store_error)?
    .ok_or(SourceImportError::ProjectNotFound)?;
    if stored.repository_url.as_deref() != Some(repository.url.as_str()) {
        return Err(SourceImportError::PointerConflict);
    }
    Ok((repository.url, commit))
}
