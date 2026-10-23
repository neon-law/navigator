//! Google Workspace Drive folders for a Project's ingest dropbox.
//!
//! This module deliberately does not store documents: working files and
//! client-facing artifacts remain [`crate::StorageService`] assets. A
//! [`DriveService`] manages the per-Project ingest folder and its Workspace
//! permissions, and exposes the bounded file read used by the operator import
//! lane. The portal never calls that read path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive";
const DRIVE_BASE_URL: &str = "https://www.googleapis.com/drive/v3";
const FOLDER_MIME_TYPE: &str = "application/vnd.google-apps.folder";
/// Maximum number of bytes a Drive download may materialize in memory.
pub const MAX_DRIVE_DOWNLOAD_BYTES: usize = 25 * 1024 * 1024;
const DRIVE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
// The importer rejects more than 50 files. Return one sentinel item beyond
// that limit so the caller can reject an oversized folder without listing it
// indefinitely or downloading any bytes.
const MAX_IMPORT_FILE_LIST: usize = 51;

/// The two entities that operate an independent Google Workspace.
///
/// The enum is configuration-only. A later Project-to-Workspace resolver owns
/// mapping a database entity to one of these deployments; Drive callers never
/// infer that mapping from a Project code or folder name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveWorkspace {
    NeonLaw,
}

impl DriveWorkspace {
    /// The per-entity prefix of the `NAVIGATOR_DRIVE_*` variables documented
    /// in `.env.example` and pinned by the workshop's Environment Matrix. The
    /// firm's prefix is spelled after its `neonlaw.com` entity zone, matching
    /// the variant; the firm's identifier is `neon`.
    const fn env_prefix(self) -> &'static str {
        match self {
            Self::NeonLaw => "NAVIGATOR_DRIVE_NEON_LAW",
        }
    }
}

/// Credentials and root shared-drive coordinates for one entity Workspace.
///
/// `service_account_json` is kept out of `Debug` so accidental diagnostics do
/// not expose a private key. The service account must be granted domain-wide
/// delegation by the owning Workspace; `delegated_user` is the Workspace user
/// it impersonates for Drive API calls.
#[derive(Clone)]
pub struct DriveWorkspaceConfig {
    pub workspace: DriveWorkspace,
    pub projects_drive_id: String,
    pub delegated_user: String,
    pub service_account_json: String,
}

impl DriveWorkspaceConfig {
    pub fn from_env(workspace: DriveWorkspace) -> Result<Self, DriveError> {
        Self::from_lookup(workspace, |key| std::env::var(key).ok())
    }

    pub fn from_lookup<F>(workspace: DriveWorkspace, get: F) -> Result<Self, DriveError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let prefix = workspace.env_prefix();
        let value = |suffix: &str| {
            let key = format!("{prefix}_{suffix}");
            let found = get(&key).filter(|value| !value.trim().is_empty());
            (key, found)
        };
        let (drive_key, drive) = value("PROJECTS_DRIVE_ID");
        let (user_key, user) = value("DELEGATED_USER");
        let (json_key, json) = value("SERVICE_ACCOUNT_JSON");
        match (drive, user, json) {
            (Some(projects_drive_id), Some(delegated_user), Some(service_account_json)) => {
                Ok(Self {
                    workspace,
                    projects_drive_id,
                    delegated_user,
                    service_account_json,
                })
            }
            (drive, user, json) => {
                // Name every missing key at once, so one run shows the whole fix.
                let missing: Vec<String> = [
                    (drive_key, drive.is_none()),
                    (user_key, user.is_none()),
                    (json_key, json.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, absent)| absent.then_some(key))
                .collect();
                Err(DriveError::MissingConfig(missing.join(", ")))
            }
        }
    }
}

impl std::fmt::Debug for DriveWorkspaceConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DriveWorkspaceConfig")
            .field("workspace", &self.workspace)
            .field("projects_drive_id", &self.projects_drive_id)
            .field("delegated_user", &"[redacted]")
            .field("service_account_json", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFolder {
    pub id: String,
    /// A Project code. The implementation only lists folders directly under
    /// the Projects shared-drive root, never working-file names.
    pub name: String,
}

/// A non-folder file in a Project's Drive ingest folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
    pub modified_time: Option<String>,
    /// Drive's own MD5 of the stored bytes: the receipt an import checks the
    /// downloaded bytes against. Absent for Google-native documents.
    pub md5_checksum: Option<String>,
    /// Drive's monotonically increasing file version.
    pub version: Option<String>,
}

/// Lowercase hex MD5 of `bytes`, in the form Drive reports `md5Checksum`.
/// Used only to compare with that receipt, never as an integrity digest of
/// Navigator's own.
#[must_use]
pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::Digest as _;
    use std::fmt::Write as _;
    md5::Md5::digest(bytes)
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Bytes fetched from Drive for an ingest operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFileDownload {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub enum DriveMemberKind {
    User,
    Group,
}

impl DriveMemberKind {
    const fn api_value(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Group => "group",
        }
    }
}

/// A Workspace principal. Its address is intentionally never attached to a
/// tracing span; callers may only emit identifiers and aggregate counts.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
pub struct DriveMember {
    pub kind: DriveMemberKind,
    pub email: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub enum DriveRole {
    Reader,
    Writer,
}

impl DriveRole {
    const fn api_value(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
        }
    }
}

#[derive(Debug, Error)]
pub enum DriveError {
    #[error("missing required Drive configuration: {0}")]
    MissingConfig(String),
    #[error("Drive authentication failed")]
    Authentication,
    #[error("Drive request failed while {action}")]
    Request {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("Drive API returned {status} while {action}")]
    Api { action: &'static str, status: u16 },
    #[error("Drive API returned an invalid response while {action}")]
    Response {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("Drive download is {actual} bytes; the maximum is {limit}")]
    DownloadTooLarge { actual: u64, limit: usize },
    #[error("Drive API response while {action} did not include a folder id")]
    MissingFolderId { action: &'static str },
    #[error("Drive API response while {action} did not include a permission id")]
    MissingPermissionId { action: &'static str },
    #[error("Drive found {count} folders for one project code; refusing to guess")]
    AmbiguousFolderName { count: usize },
}

/// The internal Workspace-folder contract. It has no document-byte methods:
/// object storage remains the canonical client asset boundary.
#[async_trait]
pub trait DriveService: Send + Sync {
    async fn create_folder(&self, project_code: &str) -> Result<DriveFolder, DriveError>;
    async fn find_folder_by_name(
        &self,
        project_code: &str,
    ) -> Result<Option<DriveFolder>, DriveError>;
    async fn list_folders(&self) -> Result<Vec<DriveFolder>, DriveError>;
    /// Whether `folder_id` is a live folder inside this deployment's Projects
    /// shared drive. Checked by id, never by name, so a Project code and its
    /// folder's display name need not agree.
    async fn is_ingest_folder(&self, folder_id: &str) -> Result<bool, DriveError>;
    /// List the files directly inside one Project's ingest folder.
    async fn list_files(&self, folder_id: &str) -> Result<Vec<DriveFile>, DriveError>;
    /// Fetch one file's bytes for the asynchronous/operator-triggered ingest
    /// lane. Portal request paths do not call this method.
    async fn download_file(&self, file_id: &str) -> Result<DriveFileDownload, DriveError>;
    async fn set_member_permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
        role: DriveRole,
    ) -> Result<(), DriveError>;
    async fn remove_member_permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
    ) -> Result<(), DriveError>;
    async fn move_to_archive(&self, folder_id: &str) -> Result<(), DriveError>;
    async fn restore_from_archive(&self, folder_id: &str) -> Result<(), DriveError>;
}

/// Production Drive v3 client authenticated as one Workspace service account
/// with domain-wide delegation.
pub struct GoogleDrive {
    projects_drive_id: String,
    token_source: Arc<dyn google_cloud_token::TokenSource>,
    http: reqwest::Client,
    base_url: String,
    request_timeout: Duration,
}

impl GoogleDrive {
    pub async fn new(config: DriveWorkspaceConfig) -> Result<Self, DriveError> {
        let credentials = google_cloud_auth::credentials::CredentialsFile::new_from_str(
            &config.service_account_json,
        )
        .await
        .map_err(|_| DriveError::Authentication)?;
        let scopes = [DRIVE_SCOPE];
        let auth_config = google_cloud_auth::project::Config::default()
            .with_scopes(&scopes)
            .with_sub(&config.delegated_user);
        let provider = google_cloud_auth::token::DefaultTokenSourceProvider::new_with_credentials(
            auth_config,
            Box::new(credentials),
        )
        .await
        .map_err(|_| DriveError::Authentication)?;
        Ok(Self::from_parts(
            config.projects_drive_id,
            google_cloud_token::TokenSourceProvider::token_source(&provider),
            DRIVE_BASE_URL,
        ))
    }

    /// Construct from `NAVIGATOR_DRIVE_NEON_LAW_*` when a deployment has
    /// configured the Workspace. Callers may skip this provider only when all
    /// of its configuration keys are absent; once any key is present,
    /// initialization errors must be reported.
    pub async fn from_env() -> Result<Self, DriveError> {
        Self::new(DriveWorkspaceConfig::from_env(DriveWorkspace::NeonLaw)?).await
    }

    fn from_parts(
        projects_drive_id: String,
        token_source: Arc<dyn google_cloud_token::TokenSource>,
        base_url: &str,
    ) -> Self {
        Self::from_parts_with_timeout(
            projects_drive_id,
            token_source,
            base_url,
            DRIVE_DOWNLOAD_TIMEOUT,
        )
    }

    fn from_parts_with_timeout(
        projects_drive_id: String,
        token_source: Arc<dyn google_cloud_token::TokenSource>,
        base_url: &str,
        request_timeout: Duration,
    ) -> Self {
        Self {
            projects_drive_id,
            token_source,
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            request_timeout,
        }
    }

    async fn token(&self) -> Result<String, DriveError> {
        let raw = self
            .token_source
            .token()
            .await
            .map_err(|_| DriveError::Authentication)?;
        Ok(raw.strip_prefix("Bearer ").unwrap_or(&raw).to_string())
    }

    fn files_url(&self) -> String {
        format!("{}/files", self.base_url)
    }

    fn file_url(&self, folder_id: &str) -> String {
        format!("{}/files/{folder_id}", self.base_url)
    }

    fn permissions_url(&self, folder_id: &str) -> String {
        format!("{}/permissions", self.file_url(folder_id))
    }

    fn checked(
        response: Result<reqwest::Response, reqwest::Error>,
        action: &'static str,
    ) -> Result<reqwest::Response, DriveError> {
        let response = response.map_err(|source| DriveError::Request { action, source })?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(DriveError::Api {
                action,
                status: response.status().as_u16(),
            })
        }
    }

    async fn archive_folder(&self) -> Result<DriveFolder, DriveError> {
        if let Some(folder) = self.find_folder_by_name("Archive").await? {
            return Ok(folder);
        }
        self.create_folder("Archive").await
    }

    async fn folder_parents(&self, folder_id: &str) -> Result<Vec<String>, DriveError> {
        let token = self.token().await?;
        let response = Self::checked(
            self.http
                .get(self.file_url(folder_id))
                .bearer_auth(token)
                .query(&[("supportsAllDrives", "true"), ("fields", "parents")])
                .send()
                .await,
            "reading folder parents",
        )?;
        response
            .json::<DriveApiFile>()
            .await
            .map(|file| file.parents.unwrap_or_default())
            .map_err(|source| DriveError::Response {
                action: "reading folder parents",
                source,
            })
    }

    async fn move_folder(&self, folder_id: &str, parent_id: &str) -> Result<(), DriveError> {
        let parents = self.folder_parents(folder_id).await?;
        if parents.iter().any(|parent| parent == parent_id) {
            tracing::info!(
                folder_id,
                outcome = "already_in_parent",
                "Drive folder move completed"
            );
            return Ok(());
        }
        let token = self.token().await?;
        let remove_parents = parents.join(",");
        Self::checked(
            self.http
                .patch(self.file_url(folder_id))
                .bearer_auth(token)
                .query(&[
                    ("supportsAllDrives", "true"),
                    ("addParents", parent_id),
                    ("removeParents", remove_parents.as_str()),
                ])
                .send()
                .await,
            "moving folder",
        )?;
        tracing::info!(folder_id, outcome = "moved", "Drive folder move completed");
        Ok(())
    }

    async fn permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
    ) -> Result<Option<DrivePermission>, DriveError> {
        let token = self.token().await?;
        let response = Self::checked(
            self.http
                .get(self.permissions_url(folder_id))
                .bearer_auth(token)
                .query(&[
                    ("supportsAllDrives", "true"),
                    ("fields", "permissions(id,emailAddress,type,role)"),
                ])
                .send()
                .await,
            "listing folder permissions",
        )?;
        let permissions = response
            .json::<DrivePermissions>()
            .await
            .map_err(|source| DriveError::Response {
                action: "listing folder permissions",
                source,
            })?;
        Ok(permissions.permissions.into_iter().find(|permission| {
            permission.email_address.as_deref() == Some(member.email.as_str())
                && permission.kind.as_deref() == Some(member.kind.api_value())
        }))
    }
}

#[async_trait]
impl DriveService for GoogleDrive {
    async fn create_folder(&self, project_code: &str) -> Result<DriveFolder, DriveError> {
        if let Some(existing) = self.find_folder_by_name(project_code).await? {
            tracing::info!(
                project_code,
                folder_id = existing.id.as_str(),
                outcome = "already_exists",
                "Drive folder provisioning completed"
            );
            return Ok(existing);
        }
        let token = self.token().await?;
        let response = Self::checked(
            self.http
                .post(self.files_url())
                .bearer_auth(token)
                .query(&[("supportsAllDrives", "true"), ("fields", "id,name")])
                .json(&CreateFolder {
                    name: project_code,
                    mime_type: FOLDER_MIME_TYPE,
                    parents: [&self.projects_drive_id],
                })
                .send()
                .await,
            "creating folder",
        )?;
        let folder =
            response
                .json::<DriveApiFile>()
                .await
                .map_err(|source| DriveError::Response {
                    action: "creating folder",
                    source,
                })?;
        let id = folder.id.ok_or(DriveError::MissingFolderId {
            action: "creating folder",
        })?;
        let created = DriveFolder {
            id,
            name: folder.name.unwrap_or_else(|| project_code.to_string()),
        };
        tracing::info!(
            project_code,
            folder_id = created.id.as_str(),
            outcome = "created",
            "Drive folder provisioning completed"
        );
        Ok(created)
    }

    async fn find_folder_by_name(
        &self,
        project_code: &str,
    ) -> Result<Option<DriveFolder>, DriveError> {
        let escaped_name = project_code.replace('\\', "\\\\").replace('\'', "\\'");
        let query = format!(
            "'{}' in parents and name = '{}' and mimeType = '{}' and trashed = false",
            self.projects_drive_id, escaped_name, FOLDER_MIME_TYPE
        );
        let token = self.token().await?;
        let response = Self::checked(
            self.http
                .get(self.files_url())
                .bearer_auth(token)
                .query(&[
                    ("q", query.as_str()),
                    ("corpora", "drive"),
                    ("driveId", self.projects_drive_id.as_str()),
                    ("includeItemsFromAllDrives", "true"),
                    ("supportsAllDrives", "true"),
                    ("fields", "files(id,name)"),
                    ("pageSize", "2"),
                ])
                .send()
                .await,
            "finding folder by name",
        )?;
        let folders = response
            .json::<DriveApiFiles>()
            .await
            .map_err(|source| DriveError::Response {
                action: "finding folder by name",
                source,
            })?
            .files
            .into_iter()
            .filter_map(DriveApiFile::into_folder)
            .collect::<Vec<_>>();
        match folders.as_slice() {
            [] => {
                tracing::info!(
                    project_code,
                    outcome = "missing",
                    "Drive folder lookup completed"
                );
                Ok(None)
            }
            [folder] => {
                tracing::info!(
                    project_code,
                    folder_id = folder.id.as_str(),
                    outcome = "found",
                    "Drive folder lookup completed"
                );
                Ok(Some(folder.clone()))
            }
            _ => {
                tracing::warn!(
                    project_code,
                    folder_count = folders.len(),
                    outcome = "ambiguous",
                    "Drive folder lookup refused to guess"
                );
                Err(DriveError::AmbiguousFolderName {
                    count: folders.len(),
                })
            }
        }
    }

    async fn list_folders(&self) -> Result<Vec<DriveFolder>, DriveError> {
        let query = format!(
            "'{}' in parents and mimeType = '{}' and trashed = false",
            self.projects_drive_id, FOLDER_MIME_TYPE
        );
        let token = self.token().await?;
        let response = Self::checked(
            self.http
                .get(self.files_url())
                .bearer_auth(token)
                .query(&[
                    ("q", query.as_str()),
                    ("corpora", "drive"),
                    ("driveId", self.projects_drive_id.as_str()),
                    ("includeItemsFromAllDrives", "true"),
                    ("supportsAllDrives", "true"),
                    ("fields", "files(id,name)"),
                    ("orderBy", "name"),
                ])
                .send()
                .await,
            "listing project folders",
        )?;
        let folders = response
            .json::<DriveApiFiles>()
            .await
            .map_err(|source| DriveError::Response {
                action: "listing project folders",
                source,
            })
            .map(|files| {
                files
                    .files
                    .into_iter()
                    .filter_map(DriveApiFile::into_folder)
                    .collect::<Vec<_>>()
            })?;
        tracing::info!(
            folder_count = folders.len(),
            outcome = "listed",
            "Drive project folders listed"
        );
        Ok(folders)
    }

    async fn is_ingest_folder(&self, folder_id: &str) -> Result<bool, DriveError> {
        let token = self.token().await?;
        let response = self
            .http
            .get(self.file_url(folder_id))
            .bearer_auth(token)
            .query(&[
                ("supportsAllDrives", "true"),
                ("fields", "id,mimeType,driveId,trashed"),
            ])
            .send()
            .await
            .map_err(|source| DriveError::Request {
                action: "reading ingest folder",
                source,
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let response = Self::checked(Ok(response), "reading ingest folder")?;
        let folder =
            response
                .json::<DriveApiFile>()
                .await
                .map_err(|source| DriveError::Response {
                    action: "reading ingest folder",
                    source,
                })?;
        Ok(folder.mime_type.as_deref() == Some(FOLDER_MIME_TYPE)
            && folder.drive_id.as_deref() == Some(self.projects_drive_id.as_str())
            && folder.trashed != Some(true))
    }

    async fn list_files(&self, folder_id: &str) -> Result<Vec<DriveFile>, DriveError> {
        let query = format!("'{folder_id}' in parents and trashed = false");
        let mut page_token: Option<String> = None;
        let mut files = Vec::new();
        loop {
            let token = self.token().await?;
            let mut request = self.http.get(self.files_url()).bearer_auth(token).query(&[
                ("q", query.as_str()),
                ("corpora", "drive"),
                ("driveId", self.projects_drive_id.as_str()),
                ("includeItemsFromAllDrives", "true"),
                ("supportsAllDrives", "true"),
                (
                    "fields",
                    "nextPageToken,files(id,name,mimeType,size,modifiedTime,md5Checksum,version)",
                ),
                ("pageSize", "51"),
            ]);
            if let Some(page_token) = page_token.as_deref() {
                request = request.query(&[("pageToken", page_token)]);
            }
            let response = Self::checked(request.send().await, "listing project files")?;
            let page =
                response
                    .json::<DriveApiFiles>()
                    .await
                    .map_err(|source| DriveError::Response {
                        action: "listing project files",
                        source,
                    })?;
            files.extend(page.files.into_iter().filter_map(DriveApiFile::into_file));
            if files.len() >= MAX_IMPORT_FILE_LIST {
                files.truncate(MAX_IMPORT_FILE_LIST);
                break;
            }
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(files)
    }

    async fn download_file(&self, file_id: &str) -> Result<DriveFileDownload, DriveError> {
        let token = self.token().await?;
        let mut response = Self::checked(
            self.http
                .get(self.file_url(file_id))
                .bearer_auth(token)
                .query(&[("alt", "media"), ("supportsAllDrives", "true")])
                .timeout(self.request_timeout)
                .send()
                .await,
            "downloading project file",
        )?;
        if let Some(content_length) = response.content_length() {
            if content_length > MAX_DRIVE_DOWNLOAD_BYTES as u64 {
                return Err(DriveError::DownloadTooLarge {
                    actual: content_length,
                    limit: MAX_DRIVE_DOWNLOAD_BYTES,
                });
            }
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map_or_else(|| "application/octet-stream".to_string(), str::to_string);
        let capacity = response.content_length().map_or(0, |length| {
            usize::try_from(length)
                .unwrap_or(usize::MAX)
                .min(MAX_DRIVE_DOWNLOAD_BYTES)
        });
        let mut bytes = Vec::with_capacity(capacity);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| DriveError::Response {
                action: "downloading project file",
                source,
            })?
        {
            let actual = bytes.len().saturating_add(chunk.len());
            if actual > MAX_DRIVE_DOWNLOAD_BYTES {
                return Err(DriveError::DownloadTooLarge {
                    actual: actual as u64,
                    limit: MAX_DRIVE_DOWNLOAD_BYTES,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(DriveFileDownload {
            bytes,
            content_type,
        })
    }

    async fn set_member_permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
        role: DriveRole,
    ) -> Result<(), DriveError> {
        let existing = self.permission(folder_id, member).await?;
        let token = self.token().await?;
        if let Some(permission) = existing {
            if permission.role.as_deref() == Some(role.api_value()) {
                tracing::info!(
                    folder_id,
                    outcome = "already_set",
                    "Drive folder permission reconciliation completed"
                );
                return Ok(());
            }
            let permission_id = permission.id.ok_or(DriveError::MissingPermissionId {
                action: "updating folder permission",
            })?;
            Self::checked(
                self.http
                    .patch(format!(
                        "{}/{}",
                        self.permissions_url(folder_id),
                        permission_id
                    ))
                    .bearer_auth(token)
                    .query(&[("supportsAllDrives", "true")])
                    .json(&PermissionRole {
                        role: role.api_value(),
                    })
                    .send()
                    .await,
                "updating folder permission",
            )?;
            tracing::info!(
                folder_id,
                outcome = "updated",
                "Drive folder permission reconciliation completed"
            );
        } else {
            Self::checked(
                self.http
                    .post(self.permissions_url(folder_id))
                    .bearer_auth(token)
                    .query(&[
                        ("supportsAllDrives", "true"),
                        ("sendNotificationEmail", "false"),
                    ])
                    .json(&CreatePermission {
                        kind: member.kind.api_value(),
                        role: role.api_value(),
                        email_address: &member.email,
                    })
                    .send()
                    .await,
                "creating folder permission",
            )?;
            tracing::info!(
                folder_id,
                outcome = "created",
                "Drive folder permission reconciliation completed"
            );
        }
        Ok(())
    }

    async fn remove_member_permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
    ) -> Result<(), DriveError> {
        let Some(permission) = self.permission(folder_id, member).await? else {
            tracing::info!(
                folder_id,
                outcome = "already_absent",
                "Drive folder permission reconciliation completed"
            );
            return Ok(());
        };
        let permission_id = permission.id.ok_or(DriveError::MissingPermissionId {
            action: "removing folder permission",
        })?;
        let token = self.token().await?;
        Self::checked(
            self.http
                .delete(format!(
                    "{}/{}",
                    self.permissions_url(folder_id),
                    permission_id
                ))
                .bearer_auth(token)
                .query(&[("supportsAllDrives", "true")])
                .send()
                .await,
            "removing folder permission",
        )?;
        tracing::info!(
            folder_id,
            outcome = "removed",
            "Drive folder permission reconciliation completed"
        );
        Ok(())
    }

    async fn move_to_archive(&self, folder_id: &str) -> Result<(), DriveError> {
        let archive = self.archive_folder().await?;
        self.move_folder(folder_id, &archive.id).await
    }

    async fn restore_from_archive(&self, folder_id: &str) -> Result<(), DriveError> {
        self.move_folder(folder_id, &self.projects_drive_id).await
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveApiFile {
    id: Option<String>,
    name: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    size: Option<String>,
    #[serde(rename = "modifiedTime")]
    modified_time: Option<String>,
    parents: Option<Vec<String>>,
    #[serde(rename = "md5Checksum")]
    md5_checksum: Option<String>,
    version: Option<String>,
    #[serde(rename = "driveId")]
    drive_id: Option<String>,
    trashed: Option<bool>,
}

impl DriveApiFile {
    fn into_folder(self) -> Option<DriveFolder> {
        Some(DriveFolder {
            id: self.id?,
            name: self.name?,
        })
    }

    fn into_file(self) -> Option<DriveFile> {
        let mime_type = self.mime_type?;
        if mime_type == FOLDER_MIME_TYPE {
            return None;
        }
        Some(DriveFile {
            id: self.id?,
            name: self.name?,
            mime_type,
            size_bytes: self.size.and_then(|size| size.parse().ok()),
            modified_time: self.modified_time,
            md5_checksum: self.md5_checksum,
            version: self.version,
        })
    }
}

#[derive(Debug, Deserialize)]
struct DriveApiFiles {
    #[serde(default)]
    files: Vec<DriveApiFile>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DrivePermission {
    id: Option<String>,
    email_address: Option<String>,
    kind: Option<String>,
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DrivePermissions {
    #[serde(default)]
    permissions: Vec<DrivePermission>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateFolder<'a> {
    name: &'a str,
    mime_type: &'a str,
    parents: [&'a String; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatePermission<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    role: &'a str,
    email_address: &'a str,
}

#[derive(Serialize)]
struct PermissionRole<'a> {
    role: &'a str,
}

/// In-memory Drive implementation for workflow and store tests. It models the
/// idempotent folder/permission semantics without requiring a Workspace.
#[derive(Clone, Default)]
pub struct FakeDrive {
    state: Arc<Mutex<FakeDriveState>>,
}

#[derive(Default)]
struct FakeDriveState {
    folders: BTreeMap<String, FakeFolder>,
    files: BTreeMap<String, FakeFile>,
    next_id: usize,
    list_files_calls: usize,
    download_calls: usize,
    failing_downloads: usize,
}

#[derive(Default)]
struct FakeFolder {
    id: String,
    archived: bool,
    permissions: BTreeSet<(DriveMember, DriveRole)>,
}

struct FakeFile {
    folder_id: String,
    file: DriveFile,
    bytes: Vec<u8>,
}

impl FakeDrive {
    /// Add synthetic bytes to a folder for an ingest test.
    #[must_use]
    pub fn add_file(
        &self,
        folder_id: &str,
        name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
        modified_time: Option<String>,
    ) -> Option<DriveFile> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        if !state.folders.values().any(|folder| folder.id == folder_id) {
            return None;
        }
        state.next_id += 1;
        let id = format!("file-{}", state.next_id);
        let file = DriveFile {
            id: id.clone(),
            name: name.to_string(),
            mime_type: mime_type.to_string(),
            size_bytes: Some(bytes.len() as u64),
            modified_time,
            md5_checksum: Some(md5_hex(&bytes)),
            version: Some("1".into()),
        };
        state.files.insert(
            id,
            FakeFile {
                folder_id: folder_id.to_string(),
                file: file.clone(),
                bytes,
            },
        );
        Some(file)
    }

    /// Replace a file's bytes as a new Drive version, refreshing its receipt.
    pub fn replace_file(&self, file_id: &str, bytes: Vec<u8>, modified_time: Option<String>) {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        let entry = state.files.get_mut(file_id).expect("fake file exists");
        let version = entry
            .file
            .version
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        entry.file.version = Some(version.to_string());
        entry.file.size_bytes = Some(bytes.len() as u64);
        entry.file.md5_checksum = Some(md5_hex(&bytes));
        entry.file.modified_time = modified_time;
        entry.bytes = bytes;
    }

    /// Make a file's listed receipt disagree with its bytes.
    pub fn set_listed_checksum(&self, file_id: &str, md5_checksum: &str) {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        state
            .files
            .get_mut(file_id)
            .expect("fake file exists")
            .file
            .md5_checksum = Some(md5_checksum.to_string());
    }

    /// Fail the next `count` downloads, as an interrupted transfer would.
    pub fn fail_downloads(&self, count: usize) {
        self.state
            .lock()
            .expect("fake drive mutex poisoned")
            .failing_downloads = count;
    }

    #[must_use]
    pub fn members(&self, folder_id: &str) -> Vec<(DriveMember, DriveRole)> {
        let state = self.state.lock().expect("fake drive mutex poisoned");
        state
            .folders
            .values()
            .find(|folder| folder.id == folder_id)
            .map(|folder| folder.permissions.iter().cloned().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn is_archived(&self, folder_id: &str) -> bool {
        let state = self.state.lock().expect("fake drive mutex poisoned");
        state
            .folders
            .values()
            .find(|folder| folder.id == folder_id)
            .is_some_and(|folder| folder.archived)
    }

    #[must_use]
    pub fn list_files_calls(&self) -> usize {
        self.state
            .lock()
            .expect("fake drive mutex poisoned")
            .list_files_calls
    }

    #[must_use]
    pub fn download_calls(&self) -> usize {
        self.state
            .lock()
            .expect("fake drive mutex poisoned")
            .download_calls
    }
}

#[async_trait]
impl DriveService for FakeDrive {
    async fn create_folder(&self, project_code: &str) -> Result<DriveFolder, DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        if let Some(folder) = state.folders.get(project_code) {
            return Ok(DriveFolder {
                id: folder.id.clone(),
                name: project_code.to_string(),
            });
        }
        state.next_id += 1;
        let id = format!("folder-{}", state.next_id);
        state.folders.insert(
            project_code.to_string(),
            FakeFolder {
                id: id.clone(),
                ..Default::default()
            },
        );
        Ok(DriveFolder {
            id,
            name: project_code.to_string(),
        })
    }

    async fn find_folder_by_name(
        &self,
        project_code: &str,
    ) -> Result<Option<DriveFolder>, DriveError> {
        let state = self.state.lock().expect("fake drive mutex poisoned");
        Ok(state.folders.get(project_code).map(|folder| DriveFolder {
            id: folder.id.clone(),
            name: project_code.to_string(),
        }))
    }

    async fn list_folders(&self) -> Result<Vec<DriveFolder>, DriveError> {
        let state = self.state.lock().expect("fake drive mutex poisoned");
        Ok(state
            .folders
            .iter()
            .filter(|(_, folder)| !folder.archived)
            .map(|(name, folder)| DriveFolder {
                id: folder.id.clone(),
                name: name.clone(),
            })
            .collect())
    }

    async fn is_ingest_folder(&self, folder_id: &str) -> Result<bool, DriveError> {
        let state = self.state.lock().expect("fake drive mutex poisoned");
        Ok(state
            .folders
            .values()
            .any(|folder| folder.id == folder_id && !folder.archived))
    }

    async fn list_files(&self, folder_id: &str) -> Result<Vec<DriveFile>, DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        state.list_files_calls += 1;
        Ok(state
            .files
            .values()
            .filter(|file| file.folder_id == folder_id)
            .map(|file| file.file.clone())
            .collect())
    }

    async fn download_file(&self, file_id: &str) -> Result<DriveFileDownload, DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        state.download_calls += 1;
        if state.failing_downloads > 0 {
            state.failing_downloads -= 1;
            return Err(DriveError::Api {
                action: "downloading project file",
                status: 503,
            });
        }
        let file = state.files.get(file_id).ok_or(DriveError::Api {
            action: "downloading project file",
            status: 404,
        })?;
        Ok(DriveFileDownload {
            bytes: file.bytes.clone(),
            content_type: file.file.mime_type.clone(),
        })
    }

    async fn set_member_permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
        role: DriveRole,
    ) -> Result<(), DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        if let Some(folder) = state
            .folders
            .values_mut()
            .find(|folder| folder.id == folder_id)
        {
            folder.permissions.retain(|(current, _)| current != member);
            folder.permissions.insert((member.clone(), role));
        }
        Ok(())
    }

    async fn remove_member_permission(
        &self,
        folder_id: &str,
        member: &DriveMember,
    ) -> Result<(), DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        if let Some(folder) = state
            .folders
            .values_mut()
            .find(|folder| folder.id == folder_id)
        {
            folder.permissions.retain(|(current, _)| current != member);
        }
        Ok(())
    }

    async fn move_to_archive(&self, folder_id: &str) -> Result<(), DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        if let Some(folder) = state
            .folders
            .values_mut()
            .find(|folder| folder.id == folder_id)
        {
            folder.archived = true;
        }
        Ok(())
    }

    async fn restore_from_archive(&self, folder_id: &str) -> Result<(), DriveError> {
        let mut state = self.state.lock().expect("fake drive mutex poisoned");
        if let Some(folder) = state
            .folders
            .values_mut()
            .find(|folder| folder.id == folder_id)
        {
            folder.archived = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        md5_hex, DriveError, DriveFolder, DriveMember, DriveMemberKind, DriveRole, DriveService,
        DriveWorkspace, DriveWorkspaceConfig, FakeDrive, GoogleDrive, FOLDER_MIME_TYPE,
        MAX_DRIVE_DOWNLOAD_BYTES, MAX_IMPORT_FILE_LIST,
    };
    use async_trait::async_trait;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Debug)]
    struct StaticToken;

    #[async_trait]
    impl google_cloud_token::TokenSource for StaticToken {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("Bearer test-token".into())
        }
    }

    /// A missing coordinate names itself, prefix included.
    ///
    /// The error text is the whole assertion: an operator reading it has to
    /// learn *which* variable to set, and a message naming only the field would
    /// send them looking through every workspace prefix.
    #[test]
    fn configuration_requires_each_workspace_coordinate() {
        let error =
            DriveWorkspaceConfig::from_lookup(DriveWorkspace::NeonLaw, |_| None).unwrap_err();
        for key in [
            "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID",
            "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
            "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON",
        ] {
            assert!(error.to_string().contains(key), "{error}");
        }
        let partial = DriveWorkspaceConfig::from_lookup(DriveWorkspace::NeonLaw, |key| {
            (key == "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID").then(|| "drive-id".to_string())
        })
        .unwrap_err()
        .to_string();
        assert!(!partial.contains("PROJECTS_DRIVE_ID"), "{partial}");
        assert!(partial.contains("DELEGATED_USER"), "{partial}");
        assert!(partial.contains("SERVICE_ACCOUNT_JSON"), "{partial}");
    }

    #[test]
    fn configuration_redacts_workspace_credentials() {
        let config = DriveWorkspaceConfig::from_lookup(DriveWorkspace::NeonLaw, |key| match key {
            "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID" => Some("drive-id".into()),
            "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER" => Some("operator@example.com".into()),
            "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON" => Some("private-key".into()),
            _ => None,
        })
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("operator@example.com"));
        assert!(!debug.contains("private-key"));
    }

    #[tokio::test]
    async fn fake_drive_converges_folder_membership_and_archive_state() {
        let drive = Arc::new(FakeDrive::default());
        let folder = drive.create_folder("matter-42").await.unwrap();
        assert_eq!(drive.create_folder("matter-42").await.unwrap(), folder);

        let member = DriveMember {
            kind: DriveMemberKind::User,
            email: "lawyer@example.com".into(),
        };
        drive
            .set_member_permission(&folder.id, &member, DriveRole::Reader)
            .await
            .unwrap();
        drive
            .set_member_permission(&folder.id, &member, DriveRole::Writer)
            .await
            .unwrap();
        assert_eq!(
            drive.members(&folder.id),
            vec![(member.clone(), DriveRole::Writer)]
        );

        drive.move_to_archive(&folder.id).await.unwrap();
        assert!(drive.is_archived(&folder.id));
        drive.restore_from_archive(&folder.id).await.unwrap();
        assert!(!drive.is_archived(&folder.id));
        drive
            .remove_member_permission(&folder.id, &member)
            .await
            .unwrap();
        assert!(drive.members(&folder.id).is_empty());
    }

    #[tokio::test]
    async fn google_drive_creates_a_folder_in_the_configured_shared_drive() {
        let server = MockServer::start().await;
        let lookup = "'projects-drive' in parents and name = 'matter-42' and mimeType = 'application/vnd.google-apps.folder' and trashed = false";
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("q", lookup))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"files": []})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .and(query_param("supportsAllDrives", "true"))
            .and(body_json(serde_json::json!({
                "name": "matter-42",
                "mimeType": "application/vnd.google-apps.folder",
                "parents": ["projects-drive"]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "folder-42", "name": "matter-42"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let drive = GoogleDrive::from_parts(
            "projects-drive".into(),
            Arc::new(StaticToken),
            &server.uri(),
        );
        assert_eq!(
            drive.create_folder("matter-42").await.unwrap().id,
            "folder-42"
        );
    }

    #[tokio::test]
    async fn google_drive_rejects_a_declared_oversized_download() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/file-oversized"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                0u8;
                MAX_DRIVE_DOWNLOAD_BYTES
                    + 1
            ]))
            .mount(&server)
            .await;
        let drive = GoogleDrive::from_parts_with_timeout(
            "projects-drive".into(),
            Arc::new(StaticToken),
            &server.uri(),
            Duration::from_secs(1),
        );

        let error = drive.download_file("file-oversized").await.unwrap_err();
        assert!(matches!(error, super::DriveError::DownloadTooLarge { .. }));
    }

    #[tokio::test]
    async fn google_drive_rejects_an_oversized_chunked_download() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/file-chunked"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0u8; MAX_DRIVE_DOWNLOAD_BYTES + 1])
                    .insert_header("transfer-encoding", "chunked"),
            )
            .mount(&server)
            .await;
        let drive = GoogleDrive::from_parts_with_timeout(
            "projects-drive".into(),
            Arc::new(StaticToken),
            &server.uri(),
            Duration::from_secs(1),
        );

        let error = drive.download_file("file-chunked").await.unwrap_err();
        assert!(matches!(error, super::DriveError::DownloadTooLarge { .. }));
    }

    #[tokio::test]
    async fn google_drive_times_out_a_slow_download() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/file-slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
            .mount(&server)
            .await;
        let drive = GoogleDrive::from_parts_with_timeout(
            "projects-drive".into(),
            Arc::new(StaticToken),
            &server.uri(),
            Duration::from_millis(10),
        );

        let error = drive.download_file("file-slow").await.unwrap_err();
        assert!(matches!(error, super::DriveError::Request { source, .. } if source.is_timeout()));
    }

    #[tokio::test]
    async fn google_drive_sets_a_user_permission_without_notification() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/folder-42/permissions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"permissions": []})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files/folder-42/permissions"))
            .and(query_param("sendNotificationEmail", "false"))
            .and(body_json(serde_json::json!({
                "type": "user",
                "role": "writer",
                "emailAddress": "lawyer@example.com"
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let drive = GoogleDrive::from_parts(
            "projects-drive".into(),
            Arc::new(StaticToken),
            &server.uri(),
        );
        drive
            .set_member_permission(
                "folder-42",
                &DriveMember {
                    kind: DriveMemberKind::User,
                    email: "lawyer@example.com".into(),
                },
                DriveRole::Writer,
            )
            .await
            .unwrap();
    }

    #[derive(Debug)]
    struct FailingToken;

    #[async_trait]
    impl google_cloud_token::TokenSource for FailingToken {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Err("token service unavailable".into())
        }
    }

    fn google(server: &MockServer) -> GoogleDrive {
        GoogleDrive::from_parts(
            "projects-drive".into(),
            Arc::new(StaticToken),
            &server.uri(),
        )
    }

    fn lawyer() -> DriveMember {
        DriveMember {
            kind: DriveMemberKind::User,
            email: "lawyer@example.com".into(),
        }
    }

    /// The receipt format is Drive's: 32 lowercase hex digits.
    #[test]
    fn md5_hex_matches_drive_receipt_format() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_ne!(md5_hex(b"abc"), md5_hex(b"abd"));
    }

    #[test]
    fn configuration_reads_every_coordinate_and_rejects_blank_values() {
        let config = DriveWorkspaceConfig::from_lookup(DriveWorkspace::NeonLaw, |key| {
            Some(format!("value-for-{key}"))
        })
        .unwrap();
        assert_eq!(config.workspace, DriveWorkspace::NeonLaw);
        assert_eq!(
            config.projects_drive_id,
            "value-for-NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID"
        );
        let blank = DriveWorkspaceConfig::from_lookup(DriveWorkspace::NeonLaw, |key| {
            (!key.ends_with("DELEGATED_USER"))
                .then(|| "x".to_string())
                .or_else(|| Some("   ".into()))
        })
        .unwrap_err()
        .to_string();
        assert!(blank.contains("DELEGATED_USER"), "{blank}");
        assert!(!blank.contains("SERVICE_ACCOUNT_JSON"), "{blank}");
    }

    #[tokio::test]
    async fn a_malformed_service_account_is_an_authentication_failure() {
        let config = DriveWorkspaceConfig {
            workspace: DriveWorkspace::NeonLaw,
            projects_drive_id: "projects-drive".into(),
            delegated_user: "operator@example.com".into(),
            service_account_json: "not json".into(),
        };
        assert!(matches!(
            GoogleDrive::new(config).await,
            Err(DriveError::Authentication)
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn fake_drive_models_versions_receipts_and_interrupted_downloads() {
        let drive = FakeDrive::default();
        assert!(drive
            .add_file(
                "folder-missing",
                "a.pdf",
                "application/pdf",
                b"a".to_vec(),
                None
            )
            .is_none());
        let folder = drive.create_folder("matter-7").await.unwrap();
        let other = drive.create_folder("matter-8").await.unwrap();
        assert_eq!(
            drive.find_folder_by_name("matter-7").await.unwrap(),
            Some(folder.clone())
        );
        assert_eq!(drive.find_folder_by_name("matter-9").await.unwrap(), None);

        let file = drive
            .add_file(
                &folder.id,
                "engagement.pdf",
                "application/pdf",
                b"version one".to_vec(),
                Some("2026-01-01T00:00:00Z".into()),
            )
            .unwrap();
        assert_eq!(file.version.as_deref(), Some("1"));
        assert_eq!(file.size_bytes, Some(11));
        assert_eq!(
            file.md5_checksum.as_deref(),
            Some(md5_hex(b"version one").as_str())
        );
        let _ = drive
            .add_file(&other.id, "other.txt", "text/plain", b"o".to_vec(), None)
            .unwrap();

        assert_eq!(
            drive.list_files(&folder.id).await.unwrap(),
            vec![file.clone()]
        );
        assert_eq!(drive.list_files_calls(), 1);

        drive.replace_file(
            &file.id,
            b"version two!".to_vec(),
            Some("2026-02-01T00:00:00Z".into()),
        );
        let listed = drive.list_files(&folder.id).await.unwrap();
        assert_eq!(listed[0].version.as_deref(), Some("2"));
        assert_eq!(listed[0].size_bytes, Some(12));
        assert_eq!(
            listed[0].md5_checksum.as_deref(),
            Some(md5_hex(b"version two!").as_str())
        );
        assert_eq!(
            listed[0].modified_time.as_deref(),
            Some("2026-02-01T00:00:00Z")
        );

        drive.set_listed_checksum(&file.id, "00000000000000000000000000000000");
        let listed = drive.list_files(&folder.id).await.unwrap();
        assert_eq!(
            listed[0].md5_checksum.as_deref(),
            Some("00000000000000000000000000000000")
        );
        assert_eq!(drive.list_files_calls(), 3);

        drive.fail_downloads(2);
        for _ in 0..2 {
            assert!(matches!(
                drive.download_file(&file.id).await,
                Err(DriveError::Api { status: 503, .. })
            ));
        }
        let download = drive.download_file(&file.id).await.unwrap();
        assert_eq!(download.bytes, b"version two!");
        assert_eq!(download.content_type, "application/pdf");
        assert_ne!(
            md5_hex(&download.bytes),
            listed[0].md5_checksum.clone().unwrap(),
            "the listed receipt now disagrees with the bytes"
        );
        assert!(matches!(
            drive.download_file("file-missing").await,
            Err(DriveError::Api { status: 404, .. })
        ));
        assert_eq!(drive.download_calls(), 4);

        assert!(drive.is_ingest_folder(&folder.id).await.unwrap());
        assert!(!drive.is_ingest_folder("folder-missing").await.unwrap());
        drive.move_to_archive(&folder.id).await.unwrap();
        assert!(!drive.is_ingest_folder(&folder.id).await.unwrap());
        assert_eq!(drive.list_folders().await.unwrap(), vec![other]);
        assert!(drive.members("folder-missing").is_empty());
        assert!(!drive.is_archived("folder-missing"));
    }

    #[tokio::test]
    async fn fake_drive_replace_counts_up_from_an_unversioned_file() {
        let drive = FakeDrive::default();
        let folder = drive.create_folder("matter-3").await.unwrap();
        let file = drive
            .add_file(&folder.id, "a.txt", "text/plain", b"a".to_vec(), None)
            .unwrap();
        drive.replace_file(&file.id, b"b".to_vec(), None);
        drive.replace_file(&file.id, b"c".to_vec(), None);
        let listed = drive.list_files(&folder.id).await.unwrap();
        assert_eq!(listed[0].version.as_deref(), Some("3"));
        assert_eq!(listed[0].modified_time, None);
    }

    #[tokio::test]
    async fn google_drive_checks_an_ingest_folder_by_id_drive_and_trash_state() {
        let server = MockServer::start().await;
        for (id, body) in [
            (
                "folder-live",
                serde_json::json!({"id": "folder-live", "mimeType": FOLDER_MIME_TYPE, "driveId": "projects-drive", "trashed": false}),
            ),
            (
                "folder-trashed",
                serde_json::json!({"id": "folder-trashed", "mimeType": FOLDER_MIME_TYPE, "driveId": "projects-drive", "trashed": true}),
            ),
            (
                "folder-elsewhere",
                serde_json::json!({"id": "folder-elsewhere", "mimeType": FOLDER_MIME_TYPE, "driveId": "another-drive"}),
            ),
            (
                "file-not-folder",
                serde_json::json!({"id": "file-not-folder", "mimeType": "application/pdf", "driveId": "projects-drive"}),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/files/{id}")))
                .and(query_param("fields", "id,mimeType,driveId,trashed"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/files/folder-gone"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-error"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-garbled"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let drive = google(&server);

        assert!(drive.is_ingest_folder("folder-live").await.unwrap());
        assert!(!drive.is_ingest_folder("folder-trashed").await.unwrap());
        assert!(!drive.is_ingest_folder("folder-elsewhere").await.unwrap());
        assert!(!drive.is_ingest_folder("file-not-folder").await.unwrap());
        assert!(!drive.is_ingest_folder("folder-gone").await.unwrap());
        assert!(matches!(
            drive.is_ingest_folder("folder-error").await,
            Err(DriveError::Api { status: 500, .. })
        ));
        assert!(matches!(
            drive.is_ingest_folder("folder-garbled").await,
            Err(DriveError::Response { .. })
        ));
    }

    #[tokio::test]
    async fn google_drive_lists_files_across_pages_with_receipts_and_skips_folders() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("pageToken", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [
                    {"id": "file-2", "name": "notes.txt", "mimeType": "text/plain"},
                ]
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param(
                "q",
                "'folder-1' in parents and trashed = false",
            ))
            .and(query_param("pageSize", "51"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "nextPageToken": "page-2",
                "files": [
                    {
                        "id": "file-1",
                        "name": "engagement.pdf",
                        "mimeType": "application/pdf",
                        "size": "2048",
                        "modifiedTime": "2026-03-01T00:00:00Z",
                        "md5Checksum": "900150983cd24fb0d6963f7d28e17f72",
                        "version": "7"
                    },
                    {"id": "sub", "name": "Subfolder", "mimeType": FOLDER_MIME_TYPE},
                    {"id": "nameless", "mimeType": "text/plain"},
                    {"id": "bad-size", "name": "x.txt", "mimeType": "text/plain", "size": "lots"}
                ]
            })))
            .with_priority(5)
            .mount(&server)
            .await;

        let files = google(&server).list_files("folder-1").await.unwrap();
        let ids: Vec<&str> = files.iter().map(|file| file.id.as_str()).collect();
        assert_eq!(ids, ["file-1", "bad-size", "file-2"]);
        assert_eq!(files[0].size_bytes, Some(2048));
        assert_eq!(
            files[0].md5_checksum.as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(files[0].version.as_deref(), Some("7"));
        assert_eq!(
            files[0].modified_time.as_deref(),
            Some("2026-03-01T00:00:00Z")
        );
        assert_eq!(files[1].size_bytes, None);
        assert_eq!(files[2].md5_checksum, None);
    }

    #[tokio::test]
    async fn google_drive_stops_listing_one_past_the_import_limit() {
        let server = MockServer::start().await;
        let page: Vec<_> = (0..60)
            .map(|index| {
                serde_json::json!({"id": format!("file-{index}"), "name": format!("{index}.txt"), "mimeType": "text/plain"})
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/files"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"nextPageToken": "more", "files": page})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let files = google(&server).list_files("folder-1").await.unwrap();
        assert_eq!(files.len(), MAX_IMPORT_FILE_LIST);
    }

    #[tokio::test]
    async fn google_drive_reports_list_failures_by_status_and_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param(
                "q",
                "'folder-denied' in parents and trashed = false",
            ))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param(
                "q",
                "'folder-garbled' in parents and trashed = false",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;
        let drive = google(&server);
        assert!(matches!(
            drive.list_files("folder-denied").await,
            Err(DriveError::Api {
                status: 403,
                action: "listing project files"
            })
        ));
        assert!(matches!(
            drive.list_files("folder-garbled").await,
            Err(DriveError::Response { .. })
        ));
    }

    #[tokio::test]
    async fn google_drive_downloads_bytes_with_their_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/file-pdf"))
            .and(query_param("alt", "media"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(b"%PDF-1.7 synthetic".to_vec(), "application/pdf"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/file-untyped"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"raw".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/file-missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let drive = google(&server);

        let pdf = drive.download_file("file-pdf").await.unwrap();
        assert_eq!(pdf.bytes, b"%PDF-1.7 synthetic");
        assert_eq!(pdf.content_type, "application/pdf");
        let untyped = drive.download_file("file-untyped").await.unwrap();
        assert_eq!(untyped.bytes, b"raw");
        assert_eq!(untyped.content_type, "application/octet-stream");
        assert!(matches!(
            drive.download_file("file-missing").await,
            Err(DriveError::Api { status: 404, .. })
        ));
    }

    #[tokio::test]
    async fn google_drive_refuses_without_a_token() {
        let drive = GoogleDrive::from_parts(
            "projects-drive".into(),
            Arc::new(FailingToken),
            "http://127.0.0.1:9/",
        );
        assert!(matches!(
            drive.download_file("file-1").await,
            Err(DriveError::Authentication)
        ));
        assert!(matches!(
            drive.is_ingest_folder("folder-1").await,
            Err(DriveError::Authentication)
        ));
        assert!(matches!(
            drive.list_files("folder-1").await,
            Err(DriveError::Authentication)
        ));
    }

    #[tokio::test]
    async fn google_drive_reports_an_unreachable_api_as_a_request_failure() {
        // Port 9 (discard) refuses connections on a test host.
        let drive = GoogleDrive::from_parts(
            "projects-drive".into(),
            Arc::new(StaticToken),
            "http://127.0.0.1:9",
        );
        assert!(matches!(
            drive.is_ingest_folder("folder-1").await,
            Err(DriveError::Request {
                action: "reading ingest folder",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn google_drive_folder_lookup_escapes_names_and_refuses_to_guess() {
        let server = MockServer::start().await;
        let escaped = "'projects-drive' in parents and name = 'o\\'brien' and mimeType = 'application/vnd.google-apps.folder' and trashed = false";
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("q", escaped))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [{"id": "folder-a", "name": "o'brien"}, {"id": "folder-b", "name": "o'brien"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("pageSize", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [{"id": "folder-42", "name": "matter-42"}]
            })))
            .with_priority(10)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let drive = google(&server);

        assert!(matches!(
            drive.find_folder_by_name("o'brien").await,
            Err(DriveError::AmbiguousFolderName { count: 2 })
        ));
        assert_eq!(
            drive.create_folder("matter-42").await.unwrap(),
            DriveFolder {
                id: "folder-42".into(),
                name: "matter-42".into()
            },
            "an existing folder is adopted, never duplicated"
        );
    }

    #[tokio::test]
    async fn google_drive_refuses_a_created_folder_without_an_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        assert!(matches!(
            google(&server).create_folder("matter-42").await,
            Err(DriveError::MissingFolderId {
                action: "creating folder"
            })
        ));
    }

    #[tokio::test]
    async fn google_drive_lists_project_folders_in_name_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("orderBy", "name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [
                    {"id": "folder-1", "name": "matter-1"},
                    {"id": "folder-2"},
                    {"id": "folder-3", "name": "matter-3"}
                ]
            })))
            .mount(&server)
            .await;
        let folders = google(&server).list_folders().await.unwrap();
        assert_eq!(
            folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["matter-1", "matter-3"]
        );
    }

    /// Drive v3 names a permission's principal kind `type`; a listed
    /// permission for some other principal never satisfies the reconcile, so
    /// the member is granted rather than assumed present.
    #[tokio::test]
    async fn google_drive_grants_a_member_absent_from_the_listed_permissions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/folder-1/permissions"))
            .and(query_param(
                "fields",
                "permissions(id,emailAddress,type,role)",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "permissions": [
                    {"id": "perm-9", "emailAddress": "paralegal@example.com", "type": "user", "role": "writer"}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files/folder-1/permissions"))
            .and(body_json(serde_json::json!({
                "type": "group",
                "role": "reader",
                "emailAddress": "team@example.com"
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let drive = google(&server);
        let team = DriveMember {
            kind: DriveMemberKind::Group,
            email: "team@example.com".into(),
        };

        drive
            .set_member_permission("folder-1", &team, DriveRole::Reader)
            .await
            .unwrap();
        // Removing a member who holds no permission is already converged.
        drive
            .remove_member_permission("folder-1", &team)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn google_drive_reports_permission_listing_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/folder-2/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-3/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("nope"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-4/permissions"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files/folder-2/permissions"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let drive = google(&server);

        drive
            .remove_member_permission("folder-2", &lawyer())
            .await
            .unwrap();
        assert!(matches!(
            drive
                .set_member_permission("folder-2", &lawyer(), DriveRole::Writer)
                .await,
            Err(DriveError::Api {
                action: "creating folder permission",
                status: 400
            })
        ));
        assert!(matches!(
            drive.remove_member_permission("folder-3", &lawyer()).await,
            Err(DriveError::Response {
                action: "listing folder permissions",
                ..
            })
        ));
        assert!(matches!(
            drive
                .set_member_permission("folder-4", &lawyer(), DriveRole::Reader)
                .await,
            Err(DriveError::Api {
                action: "listing folder permissions",
                status: 403
            })
        ));
    }

    #[tokio::test]
    async fn google_drive_archives_into_an_existing_archive_folder_and_restores() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("pageSize", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [{"id": "folder-archive", "name": "Archive"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-42"))
            .and(query_param("fields", "parents"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"parents": ["projects-drive"]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/files/folder-42"))
            .and(query_param("addParents", "folder-archive"))
            .and(query_param("removeParents", "projects-drive"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let drive = google(&server);

        drive.move_to_archive("folder-42").await.unwrap();
        // The folder already sits under the shared-drive root, so a restore
        // is a no-op rather than a second PATCH.
        drive.restore_from_archive("folder-42").await.unwrap();
    }

    #[tokio::test]
    async fn google_drive_creates_the_archive_folder_on_first_use() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"files": []})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .and(body_json(serde_json::json!({
                "name": "Archive",
                "mimeType": FOLDER_MIME_TYPE,
                "parents": ["projects-drive"]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "folder-archive"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/files/folder-42"))
            .and(query_param("addParents", "folder-archive"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/files/folder-garbled"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{"))
            .mount(&server)
            .await;
        let drive = google(&server);

        assert!(matches!(
            drive.move_to_archive("folder-42").await,
            Err(DriveError::Api {
                action: "moving folder",
                status: 500
            })
        ));
        assert!(matches!(
            drive.restore_from_archive("folder-garbled").await,
            Err(DriveError::Response {
                action: "reading folder parents",
                ..
            })
        ));
    }
}
