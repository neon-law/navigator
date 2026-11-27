//! Project-document HTTP surface:
//!
//! - `POST /app/projects/{project_code}/documents/upload` — multipart upload
//!   that pipes bytes through [`store::assets::file_revision`], filing each
//!   file as a revision of the document its trimmed filename names. The
//!   picker is `multiple`, so one submission may carry a batch of files;
//!   each becomes its own document (or the next revision of an existing
//!   one).
//! - `POST /app/projects/{project_code}/documents/drive-import` — copies the
//!   Project's optional Drive dropbox into Navigator storage through
//!   [`store::drive_import`], screening every file with the attachment scanner,
//!   and answers the per-file report as JSON.
//! - `GET /app/projects/{project_code}/documents/:doc_id` — per-document
//!   detail page showing full provenance.
//! - `GET /app/projects/{project_code}/documents/:doc_id/download` — streams
//!   the object's bytes back through the app, same-origin. Add `?inline=1`
//!   to ask for `Content-Disposition: inline` instead of `attachment`; the
//!   route grants that only for the passive content types in
//!   [`INLINE_CONTENT_TYPES`].
//!
//! # Authorization model
//!
//! Three layers gate every request before bytes leave the building:
//!
//! 1. **Admin sub-router** — CSRF, session, and embedded Rego policy are
//!    already enforced by middleware before any handler in this
//!    module runs. An unauthenticated request never reaches us.
//! 2. **Cross-project + visibility guard** — every handler resolves
//!    the document via [`load_doc_for_project`] and 404s if
//!    `document.project_id` doesn't match the `:id` segment of the
//!    URL. A user who guesses or steals a `doc_id` from another
//!    project can't tunnel it through their own project's URL. Under
//!    the client lens it also 404s an
//!    `internal` asset, so a client can't fetch internal work product
//!    on their own matter by guessing a `doc_id` — matching the gate
//!    the filename list and ZIP export already apply (#782).
//! 3. **Same-origin delivery** — only after layers 1+2 pass does the
//!    handler read the object and write its bytes into the response.
//!    No storage URL ever reaches the browser, so there is no
//!    credential to leak through history, a screenshot, or a paste,
//!    and no redirect hop for the session to lose.
//!
//! # Why the bytes proxy rather than redirect (ENG-651 / LAW-22)
//!
//! This route used to answer `307` with a `Location` on the storage
//! origin. That closed *both* ways a Project portal could render a
//! filed document: `pdf.js` fetches the PDF and the bucket publishes no
//! CORS policy, and an `<iframe>` re-evaluates CSP on the redirect hop
//! against a policy whose `frame-src` falls back to `default-src
//! 'self'`. A top-level "Download" link still worked, so the feature
//! looked wired up and only the viewer was a blank frame.
//!
//! [`docs/signed-url-delivery-audit.md`] Finding 7 chose proxying over
//! admitting the storage origin to the portal CSP. Naming
//! `storage.googleapis.com` in `connect-src` would admit *every* bucket
//! on that shared host, not ours, to third-party portal bundles — and
//! rendering inline would need `object-src 'none'` widened too, which is
//! one of the policy's two clickjacking backstops. So `PORTAL_CSP` is
//! deliberately unchanged by this route.
//!
//! Two consequences the code has to carry:
//!
//! - **Egress and CPU move to the pod**, and [`stream_through`] buffers
//!   the whole object before responding. That is bounded by what upload
//!   admits ([`MAX_BATCH_BYTES`], 500 MB), which is a real ceiling on
//!   one request's memory. A true streaming body needs a `StorageService`
//!   method that does not exist yet; tracked separately.
//! - **Inline delivery of a caller-typed body is a same-origin script
//!   vector.** An asset's `content_type` is whatever the uploader said —
//!   the multipart part's own type here, or an extension map in
//!   `cli`'s document sync — and ingest validates the `kind`, not the
//!   type. Served from the storage origin a `text/html` document
//!   executed somewhere holding none of Navigator's cookies; proxied
//!   same-origin and served `inline`, the same bytes would execute as
//!   Navigator. Hence [`INLINE_CONTENT_TYPES`], `nosniff`, and a
//!   `sandbox` CSP on every streamed response.

use axum::body::Body;
use axum::extract::{Extension, Multipart, Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use store::documents::{source, DocumentIdentity, IngestArgs};
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::admin::AdminState;
use crate::session::SessionData;

/// Content types a proxied document may be served `Content-Disposition:
/// inline` for.
///
/// Passive formats only. An asset's `content_type` is whatever the
/// uploader declared, and a document served `inline` renders in
/// Navigator's own origin, so anything the browser will execute has to
/// arrive as `attachment` no matter what the caller asked for.
///
/// `image/svg+xml` is deliberately absent. It is an image by name and a
/// scriptable document by specification, which makes it exactly the
/// entry someone adds by pattern-matching on the others.
const INLINE_CONTENT_TYPES: &[&str] = &[
    "application/pdf",
    "image/apng",
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
];

/// Most files one submission may carry: [`store::documents::MAX_UPLOAD_FORM_FILES`].
const MAX_BATCH_FILES: usize = store::documents::MAX_UPLOAD_FORM_FILES;

/// Most bytes one submission may carry across every file in it:
/// [`store::documents::MAX_UPLOAD_FORM_BYTES`].
///
/// `pub(crate)` so `admin.rs` can raise Axum's own default ~2 MB body
/// limit to match on this route — without that layer, Axum rejects a
/// multi-megabyte PDF long before this constant's check ever runs.
pub(crate) const MAX_BATCH_BYTES: usize = store::documents::MAX_UPLOAD_FORM_BYTES;

/// `POST /app/projects/{project_code}/documents/upload`.
pub async fn upload(
    State(state): State<AdminState>,
    AxumPath(project_code): AxumPath<String>,
    cookies: Cookies,
    session: Option<Extension<SessionData>>,
    mut multipart: Multipart,
) -> Response {
    let Some(Extension(session_data)) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !store::projects::is_valid_code(&project_code) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let project_id =
        match store::projects::find_by_code(&state.application_store, &project_code).await {
            Ok(Some(project)) => project.id,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(error) => {
                tracing::error!(error = %error, "document upload: Project lookup failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

    // A matter-surface write: the matter's gate (a participation row of every
    // tier) plus the lawyer-tier check, which that gate does not make — a client
    // on their own matter reaches the page but must not file to it.
    if !session_data.role.is_lawyer_tier()
        || !store::access::can_see_project(
            &state.application_store,
            session_data.person_id,
            session_data.role,
            project_id,
        )
        .await
        .unwrap_or(false)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Bail early if the project doesn't exist, so a bad id is a friendly 404
    // rather than a later failure mid-ingest.
    match store::projects::find_by_id(&state.application_store, project_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(error = %error, %project_id, "document upload: Project read failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // The multipart CSRF check the middleware can't do without buffering
    // the upload: the form renders `_csrf` first, so verify it before
    // reading the file field. Bearer callers carry no cookie and are
    // exempt, matching `require_csrf`.
    if let Err(status) =
        crate::csrf::require_multipart_csrf(&cookies, &session_data, &mut multipart).await
    {
        return status.into_response();
    }

    let batch = match read_upload_batch(&mut multipart).await {
        Ok(batch) => batch,
        Err(BatchError::Malformed) => return StatusCode::BAD_REQUEST.into_response(),
        Err(BatchError::TooLarge) => {
            tracing::warn!(%project_id, "project document upload exceeded the batch ceiling");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                store::documents::upload_form_limit_message(),
            )
                .into_response();
        }
    };
    if batch.files.is_empty() {
        return Redirect::to(&project_documents_path(&state.application_store, project_id).await)
            .into_response();
    }
    let UploadBatch {
        files: uploads,
        kind,
        description,
        visibility,
    } = batch;
    let description_trimmed = description.as_deref();

    // Ingest sequentially: each file is its own `assets` row, so a batch of
    // five files a matter as five single uploads would. A failure part-way
    // through leaves the already-filed documents in place rather than
    // rolling the batch back — the lawyer sees what landed and can re-send
    // the rest, which beats silently discarding good uploads.
    //
    // That only works because re-sending is safe: `already_filed` skips a
    // file this project already holds under the same name and content
    // hash, so retrying the whole batch tops up the missing documents
    // instead of duplicating the ones that landed the first time.
    for upload in &uploads {
        if let Err(response) = file_one(
            &state,
            project_id,
            upload,
            &kind,
            description_trimmed,
            visibility,
        )
        .await
        {
            return response;
        }
    }

    Redirect::to(&project_documents_path(&state.application_store, project_id).await)
        .into_response()
}

/// Where an upload lands: the matter's Documents tab, which renders the table
/// the batch just added to. The workbench opens on Overview (#176), which
/// lists no documents.
async fn project_documents_path(
    application_store: &store::application_store::ApplicationStore,
    project_id: uuid::Uuid,
) -> String {
    format!(
        "{}/{}",
        crate::dioxus_app::project_show_path(application_store, project_id).await,
        webapp::sections::page::MatterTab::Documents.segment()
    )
}

/// The form fields of a Drive import.
#[derive(Debug, serde::Deserialize)]
pub struct DriveImportForm {
    #[serde(default = "unclassified")]
    kind: String,
    /// `internal` or `client`; stated explicitly like every ingest.
    visibility: String,
    #[serde(default)]
    description: Option<String>,
    /// Plan and never write: the report is exactly what a real pass would
    /// file, leave unchanged, and refuse (LAW-216).
    #[serde(default)]
    dry_run: bool,
}

fn unclassified() -> String {
    "unclassified".into()
}

/// The attachment scanner as the Drive importer's malware screen.
struct ScannerScreen<'a>(&'a dyn crate::attachment_scanner::AttachmentScanner);

#[async_trait::async_trait]
impl store::drive_import::IngestScreen for ScannerScreen<'_> {
    async fn is_clean(&self, bytes: &[u8]) -> Result<bool, ()> {
        match self.0.scan(bytes).await {
            Ok(crate::attachment_scanner::ScanVerdict::Clean) => Ok(true),
            Ok(crate::attachment_scanner::ScanVerdict::Found { .. }) => Ok(false),
            Err(error) => {
                tracing::warn!(%error, "Drive import screen unavailable");
                Err(())
            }
        }
    }
}

/// `POST /app/projects/{project_code}/documents/drive-import`.
///
/// Lawyer tier on the matter, else 404. A deployment without Drive ingest
/// answers 409; a partially configured one 503. The JSON report lists what
/// was filed, what was unchanged, and what was refused and why. With
/// `dry_run` set the pass writes nothing and reports what a real pass would
/// do.
pub async fn drive_import(
    State(state): State<AdminState>,
    AxumPath(project_code): AxumPath<String>,
    session: Option<Extension<SessionData>>,
    axum::Form(form): axum::Form<DriveImportForm>,
) -> Response {
    let Some(Extension(session_data)) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(project_id) =
        store::projects::id_for_code(&state.application_store, &project_code).await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let drive = match cloud::DriveWorkspaceConfig::from_env(cloud::DriveWorkspace::NeonLaw) {
        Ok(config) => match cloud::GoogleDrive::new(config).await {
            Ok(drive) => drive,
            Err(error) => {
                tracing::error!(%error, "Drive ingest client could not start");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Drive ingest is unavailable",
                )
                    .into_response();
            }
        },
        Err(_) if drive_settings_absent() => {
            return (
                StatusCode::CONFLICT,
                "Drive ingest is not configured for this deployment",
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(%error, "Drive ingest is partially configured");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Drive ingest is misconfigured",
            )
                .into_response();
        }
    };
    drive_import_with(&state, project_id, &session_data, &drive, &form, None).await
}

/// Whether the deployment declares no Projects shared drive. The delegation
/// keys are shared with Calendar and Contacts, so only the drive declares
/// Drive ingest.
fn drive_settings_absent() -> bool {
    std::env::var("NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID")
        .map_or(true, |value| value.trim().is_empty())
}

/// The authorization and import behind [`drive_import`], with the Drive client
/// supplied.
async fn drive_import_with(
    state: &AdminState,
    project_id: Uuid,
    session: &SessionData,
    drive: &dyn cloud::DriveService,
    form: &DriveImportForm,
    folders: Option<&store::postgres::PostgresDb>,
) -> Response {
    let Some(person_id) = session.person_id else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !session.role.is_lawyer_tier()
        || !store::access::can_see_project(
            &state.application_store,
            Some(person_id),
            session.role,
            project_id,
        )
        .await
        .unwrap_or(false)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let visibility = match form.visibility.as_str() {
        store::documents::visibility::INTERNAL => store::documents::visibility::INTERNAL,
        store::documents::visibility::CLIENT => store::documents::visibility::CLIENT,
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "visibility is internal or client",
            )
                .into_response()
        }
    };
    let screen = ScannerScreen(state.attachment_scanner.as_ref());
    let args = store::drive_import::DriveImportArgs {
        kind: &form.kind,
        visibility,
        description: form
            .description
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        dry_run: form.dry_run,
    };
    let outcome = if let Some(pg) = folders {
        store::drive_import::import_project_files_with_folders(
            &state.application_store,
            pg,
            &state.storage,
            drive,
            &screen,
            project_id,
            person_id,
            &args,
        )
        .await
    } else {
        store::drive_import::import_project_files(
            &state.application_store,
            &state.storage,
            drive,
            &screen,
            project_id,
            person_id,
            &args,
        )
        .await
    };
    match outcome {
        Ok(report) => axum::Json(report).into_response(),
        Err(store::drive_import::DriveImportError::Unauthorized) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(
            error @ (store::drive_import::DriveImportError::MissingFolder
            | store::drive_import::DriveImportError::FolderMismatch),
        ) => (StatusCode::CONFLICT, error.to_string()).into_response(),
        Err(
            error @ (store::drive_import::DriveImportError::TooManyFiles { .. }
            | store::drive_import::DriveImportError::FileTooLarge { .. }
            | store::drive_import::DriveImportError::BatchTooLarge),
        ) => (StatusCode::PAYLOAD_TOO_LARGE, error.to_string()).into_response(),
        Err(store::drive_import::DriveImportError::Asset(error)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()).into_response()
        }
        Err(error) => {
            tracing::error!(%error, %project_id, "Drive import stopped");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Drive import stopped; run it again",
            )
                .into_response()
        }
    }
}

/// File one document from a batch, skipping it when the matter already
/// holds it. `Err(response)` is the response the caller must return.
async fn file_one(
    state: &AdminState,
    project_id: Uuid,
    upload: &UploadedFile,
    kind: &str,
    description: Option<&str>,
    visibility: &str,
) -> Result<(), Response> {
    let file_name = upload
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| format!("upload-{project_id}"), str::to_string);
    if store::documents::validate_document_slug(&file_name, &file_name).is_err() {
        tracing::warn!(%project_id, filename = %file_name, "refusing a document filename without an extension");
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let content_type = upload
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream");

    match already_filed(
        &state.application_store,
        project_id,
        &file_name,
        &upload.bytes,
    )
    .await
    {
        Ok(Some(_existing)) => {
            tracing::info!(
                %project_id,
                filename = %file_name,
                "skipping a project document this matter already holds"
            );
            // The bytes and filename are unchanged, so re-ingesting would
            // only duplicate the row — but a lawyer re-submitting the same
            // file under a new visibility choice must not have that choice
            // silently dropped. A matter can hold several duplicate rows for
            // one file (uploads filed before dedup existed, concurrent
            // submissions, or other ingest paths), and the dedup lookup
            // returns only one of them, so sync EVERY matching row — updating
            // just one would leave a duplicate at its old visibility and its
            // filename/bytes reachable through the client list and ZIP export
            // (#782 follow-up: Greptile P1).
            if let Err(e) = sync_visibility(
                &state.application_store,
                project_id,
                &file_name,
                &upload.bytes,
                visibility,
            )
            .await
            {
                tracing::error!(
                    error = %e, %project_id, filename = %file_name,
                    "failed to sync visibility on a re-upload of an existing document"
                );
                return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            return Ok(());
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, %project_id, filename = %file_name, "dedup lookup failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }

    let args = IngestArgs {
        project_id,
        source: source::UPLOAD,
        filename: &file_name,
        kind,
        content_type,
        description,
        secondary_storage_key: None,
        visibility,
    };

    // The document identity a browser upload files under: the trimmed
    // filename, the same default the CLI's `--slug` documents. That makes a
    // re-upload of `notice.pdf` the next revision of the same document
    // instead of an unrelated one-off row — the same lane rules
    // `store::assets::file_revision` already enforces for the CLI's own
    // `navigator project sync` path.
    let identity = DocumentIdentity {
        slug: Some(&file_name),
        published_at: None,
        metadata: None,
        derived_from: None,
        transcript_quality: None,
        signing: None,
        notarization: Vec::new(),
        evidence: None,
        folder: None,
    };
    let result = store::assets::file_revision(
        &state.application_store,
        &state.storage,
        &args,
        &identity,
        &upload.bytes,
    )
    .await;
    file_revision_response(state, project_id, &file_name, result).await
}

/// Turn a [`store::assets::file_revision`] result into what `file_one` must
/// return: `Ok` for a filed (or no-op identical) revision, `Err(response)`
/// otherwise.
async fn file_revision_response(
    state: &AdminState,
    project_id: Uuid,
    file_name: &str,
    result: Result<store::assets::Filed, store::assets::RevisionError>,
) -> Result<(), Response> {
    match result {
        Ok(_filed) => Ok(()),
        // Rule 2 of `file_revision`: kind is immutable across a chain. This is
        // the one lane rule that can reject a file the form otherwise accepts,
        // so it renders as a flash the lawyer can act on rather than a bare
        // 500.
        Err(store::assets::RevisionError::KindChanged {
            slug,
            existing,
            attempted,
        }) => {
            let message = format!(
                "`{slug}` is already filed as `{existing}`; a `{attempted}` upload is a different document."
            );
            let path = project_documents_path(&state.application_store, project_id).await;
            Err(Redirect::to(&format!(
                "{path}?error={}",
                crate::admin::encode_query_value(&message)
            ))
            .into_response())
        }
        // An invalid kind is a bad request, not a server fault: the upload
        // form's `<select>` cannot produce one, so this only fires for a
        // direct/bearer caller sending an unrecognized asset-lane kind.
        Err(e @ store::assets::RevisionError::KindNotFilable(_)) => {
            tracing::warn!(
                project_id = %project_id,
                filename = %file_name,
                error = %e,
                "project document upload rejected an unrecognized kind"
            );
            Err(StatusCode::BAD_REQUEST.into_response())
        }
        Err(e) => {
            tracing::error!(
                project_id = %project_id,
                filename = %file_name,
                error = %e,
                "project document upload failed"
            );
            Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

/// Why a multipart upload body could not be turned into an
/// [`UploadBatch`].
enum BatchError {
    /// The multipart stream itself was unreadable.
    Malformed,
    /// The batch broke [`MAX_BATCH_FILES`] or [`MAX_BATCH_BYTES`].
    TooLarge,
}

impl BatchError {
    /// A multipart read error: the route's body limit is the batch byte
    /// ceiling reached before the handler's own count, so it is
    /// [`BatchError::TooLarge`]; anything else is unreadable.
    fn from_multipart(error: &axum::extract::multipart::MultipartError) -> Self {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Self::TooLarge
        } else {
            Self::Malformed
        }
    }
}

/// The existing document `assets` row, when this project already holds one
/// with the same filename and the same bytes.
///
/// This is what makes re-sending a partially-failed batch safe. Ingest
/// dedupes the stored *object* by content hash but still writes a fresh
/// `assets` row every call, so without this check a retry would leave
/// the matter showing two rows for one file. Filename is part of the key
/// because the same bytes filed under two names are two documents.
async fn already_filed(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    filename: &str,
    bytes: &[u8],
) -> Result<Option<store::assets::Asset>, store::assets::AssetError> {
    let sha_hex = store::documents::sha256_hex(bytes);
    store::assets::find_filed_copy(application_store, project_id, filename, &sha_hex).await
}

/// Sync `visibility` across *every* row this matter holds for a re-uploaded
/// document (same filename, same bytes), used when a lawyer re-uploads a
/// document the matter already holds but picks a different visibility. The
/// dedup skip must not drop that explicit choice, and because a matter can
/// hold several duplicate rows for one file (uploads filed before dedup
/// existed, concurrent submissions, or other ingest paths), updating only
/// the one row the dedup lookup happened to return would leave a duplicate
/// at its old visibility — a re-upload as `internal` could otherwise leave
/// a duplicate still client-visible and its bytes reachable through the
/// client list and ZIP export. The `visibility != target` filter touches
/// only the divergent duplicates, so this is a no-op when they already match.
async fn sync_visibility(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    filename: &str,
    bytes: &[u8],
    visibility: &str,
) -> Result<(), store::assets::AssetError> {
    let sha_hex = store::documents::sha256_hex(bytes);
    store::assets::sync_visibility(
        application_store,
        project_id,
        filename,
        &sha_hex,
        visibility,
    )
    .await?;
    Ok(())
}

/// One file part pulled off a multipart upload, held until the whole
/// batch has been read off the wire.
struct UploadedFile {
    file_name: Option<String>,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

/// Everything one submission of the upload form carries: the files the
/// picker selected plus the batch-level metadata that applies to all of
/// them.
struct UploadBatch {
    files: Vec<UploadedFile>,
    /// Already defaulted to `unclassified` when the form left it blank.
    kind: String,
    /// `None` when the form left it blank, so it isn't stored as `""`.
    description: Option<String>,
    /// `store::documents::visibility::INTERNAL` or `::CLIENT`. Already
    /// defaulted to `INTERNAL` when the form left it blank or sent anything
    /// else — the safe tier wins over a malformed submission.
    visibility: &'static str,
}

/// Drain the multipart body into an [`UploadBatch`].
///
/// The picker is `multiple`, so the browser posts one `file` part per
/// selected file, all under the same field name — this collects every
/// one rather than taking the first. `kind` and `description` are
/// single-valued and apply to the whole batch.
///
/// [`BatchError::Malformed`] means the multipart stream was unreadable
/// (a `400`); [`BatchError::TooLarge`] means it broke a batch ceiling
/// (a `413`).
async fn read_upload_batch(multipart: &mut Multipart) -> Result<UploadBatch, BatchError> {
    let mut files: Vec<UploadedFile> = Vec::new();
    let mut kind: Option<String> = None;
    let mut description: Option<String> = None;
    let mut visibility: Option<String> = None;
    let mut total_bytes: usize = 0;

    loop {
        let next = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => return Err(BatchError::from_multipart(&error)),
        };
        let name = next.name().map(String::from);
        match name.as_deref() {
            Some("file") => {
                let file_name = next.file_name().map(String::from);
                let content_type = next.content_type().map(String::from);
                let bytes = match next.bytes().await {
                    Ok(b) => b.to_vec(),
                    Err(error) => return Err(BatchError::from_multipart(&error)),
                };
                // A picker with nothing selected still posts one part —
                // no filename, zero bytes. That is "nothing selected".
                // A *named* part is a real selection and is kept even when
                // it is empty, so a zero-byte file the lawyer chose
                // is filed rather than vanishing from the batch without a
                // word.
                let named = file_name
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|n| !n.is_empty());
                if !named && bytes.is_empty() {
                    continue;
                }

                total_bytes = total_bytes.saturating_add(bytes.len());
                if files.len() >= MAX_BATCH_FILES || total_bytes > MAX_BATCH_BYTES {
                    return Err(BatchError::TooLarge);
                }

                files.push(UploadedFile {
                    file_name,
                    content_type,
                    bytes,
                });
            }
            Some("kind") => kind = next.text().await.ok(),
            Some("description") => description = next.text().await.ok(),
            Some("visibility") => visibility = next.text().await.ok(),
            _ => {}
        }
    }

    Ok(UploadBatch {
        files,
        kind: kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| "unclassified".to_string(), str::to_string),
        description: description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        visibility: match visibility.as_deref().map(str::trim) {
            Some(store::documents::visibility::CLIENT) => store::documents::visibility::CLIENT,
            _ => store::documents::visibility::INTERNAL,
        },
    })
}

/// How the streamed response asks the browser to treat the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Disposition {
    /// Save it. The default, and the only answer for a type the browser
    /// might execute.
    Attachment,
    /// Render it in place, for the portal's embedded viewer.
    Inline,
}

impl Disposition {
    /// What the caller asked for, narrowed to what the content type earns.
    ///
    /// `inline` is a request, never an instruction: a caller that asks for
    /// it on a `text/html` document gets `attachment` anyway. The allowlist
    /// is the decision, not the query string.
    fn resolve(requested_inline: bool, content_type: &str) -> Self {
        // A stored type may carry parameters (`text/html; charset=utf-8`);
        // compare the essence so a parameter cannot smuggle a type past the
        // allowlist or keep an allowed one out of it.
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if requested_inline && INLINE_CONTENT_TYPES.contains(&essence.as_str()) {
            Self::Inline
        } else {
            Self::Attachment
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Attachment => "attachment",
            Self::Inline => "inline",
        }
    }
}

/// `?inline=1` on the download route.
#[derive(serde::Deserialize, Default)]
pub struct DownloadQuery {
    /// Present and truthy asks for `Content-Disposition: inline`.
    ///
    /// Typed as a string rather than a `bool` so `?inline=1` — what a
    /// hand-written portal link actually spells — parses. A `bool` field
    /// would reject it and 400 the whole request.
    inline: Option<String>,
    /// The fingerprinted, time-bound link's parameters
    /// ([`webapp::document_link`]). All absent is a plain session-gated
    /// download; any present must verify.
    fp: Option<String>,
    expires: Option<String>,
    sig: Option<String>,
}

impl DownloadQuery {
    /// The signed-link parameters, or `None` when the request carries none.
    fn link(&self) -> Option<webapp::document_link::LinkParams> {
        let params = webapp::document_link::LinkParams {
            fp: self.fp.clone(),
            expires: self.expires.as_deref().and_then(|e| e.parse().ok()),
            sig: self.sig.clone(),
        };
        (params.is_present() || self.expires.is_some()).then_some(params)
    }

    fn wants_inline(&self) -> bool {
        matches!(
            self.inline.as_deref(),
            Some("1" | "true" | "yes" | "inline" | "")
        )
    }
}

/// `GET /app/projects/{project_code}/documents/:doc_id/download`. Resolves
/// the document, blocks cross-project leakage, then streams the bytes
/// back same-origin.
pub async fn download(
    State(state): State<AdminState>,
    AxumPath((project_code, doc_id)): AxumPath<(String, Uuid)>,
    axum::extract::Query(query): axum::extract::Query<DownloadQuery>,
    session: Option<Extension<SessionData>>,
) -> Response {
    // A code naming no matter is the same 404 a caller off the matter gets. A
    // store *failure* is not a miss, though: it stays a 500, so an outage does
    // not read to every client as "your document is gone".
    let project_id = match store::projects::find_by_code(&state.application_store, &project_code)
        .await
    {
        Ok(Some(project)) => project.id,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(error = %error, %project_code, "project document download: matter lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let lens = match project_document_lens(&state, session.as_deref(), project_id).await {
        Ok(Some(lens)) => lens,
        Ok(None) => {
            tracing::info!(%project_id, %doc_id, "project document download denied by access policy");
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, %project_id, %doc_id, "project document access check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // Resolve the lens from the caller's participation row, not just their
    // system tier. A lawyer who is also a client on this matter must remain
    // within the client visibility boundary.
    let doc = match load_doc_for_project(&state, project_id, doc_id, lens).await {
        Ok(Some(asset)) => asset,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(
                error = %e,
                %project_id,
                %doc_id,
                "db error loading project document for download"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // A signed link must verify against this exact document: its matter, id,
    // recorded SHA-256, and deadline. Checked after the guards above, so a
    // refusal here never tells a caller off the matter that the document
    // exists.
    let cache_control = match query.link() {
        None => None,
        Some(params) => {
            let signer = state.sessions.document_link_signer();
            match signer.verify(
                &params,
                &project_code,
                &doc_id.to_string(),
                &doc.sha256_hex,
                webapp::document_link::now_unix(),
            ) {
                // The bytes behind a valid link cannot change before it
                // expires, so the browser may keep them that long.
                Ok(left) => Some(format!("private, max-age={left}, immutable")),
                Err(webapp::document_link::LinkRefusal::Expired) => {
                    return StatusCode::GONE.into_response();
                }
                Err(webapp::document_link::LinkRefusal::FingerprintMismatch) => {
                    return StatusCode::NOT_FOUND.into_response();
                }
                Err(webapp::document_link::LinkRefusal::Invalid) => {
                    tracing::warn!(%project_id, %doc_id, "project document download: invalid signed link");
                    return StatusCode::FORBIDDEN.into_response();
                }
            }
        }
    };
    let mut response = deliver(&state, &doc, query.wants_inline()).await;
    if let Some(value) = cache_control
        .filter(|_| response.status() == StatusCode::OK)
        .and_then(|value| axum::http::HeaderValue::from_str(&value).ok())
    {
        response.headers_mut().insert(header::CACHE_CONTROL, value);
    }
    response
}

/// The document's bytes, from active storage or — for a soft-deleted revision
/// the retention lane already moved — its verified archived copy.
async fn deliver(state: &AdminState, doc: &store::assets::Asset, wants_inline: bool) -> Response {
    let filename = doc.filename.as_deref().unwrap_or("document");
    let disposition = Disposition::resolve(wants_inline, &doc.content_type);

    // A soft-deleted revision whose active copy the retention lane already
    // moved is read from its verified archived copy.
    if doc.deleted_at.is_some()
        && matches!(
            state.storage.get(&doc.storage_key).await,
            Err(cloud::StorageError::NotFound(_))
        )
    {
        return match archived_bytes(doc).await {
            Some(bytes) => bytes_response(bytes, &doc.content_type, filename, disposition),
            None => StatusCode::NOT_FOUND.into_response(),
        };
    }
    stream_through(
        state.clone(),
        &doc.storage_key,
        &doc.content_type,
        filename,
        disposition,
    )
    .await
}

/// The verified archived bytes of a soft-deleted revision, when the retention
/// lanes are configured and hold them.
async fn archived_bytes(doc: &store::assets::Asset) -> Option<Vec<u8>> {
    let pg = store::postgres::PostgresDb::from_env().await.ok()?;
    let archive = cloud::archived_projects_from_env().await.ok()?;
    match store::document_retention::read_archived(&pg, &archive, doc).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, doc_id = %doc.id, "archived revision read failed");
            None
        }
    }
}

/// `Ok(None)` is a refusal; `Err` is a store failure.
///
/// These are deliberately different answers. Collapsing a failed access query
/// into "not a participant" reports a pool or query outage as a missing
/// document, which is the masking `project_document_*_500s_on_database_error`
/// exists to prevent — and it matters more now that the gate itself reads the
/// participation ledger, so an outage breaks the gate before the lookup.
async fn project_document_lens(
    state: &AdminState,
    session: Option<&SessionData>,
    project_id: Uuid,
) -> Result<Option<store::access::ProjectLens>, String> {
    let Some(session) = session else {
        return Ok(None);
    };
    store::access::matter_lens(
        &state.application_store,
        session.person_id,
        session.role,
        project_id,
    )
    .await
}

/// Look up the document `assets` row by id and reject if it doesn't
/// belong to the project_id from the URL — this is the cross-project
/// leakage guard. A bare content asset (no `project_id`) is never a
/// project document, so it fails the guard too.
///
/// The client lens additionally never resolves an `internal` asset: the
/// filename list and ZIP export are already gated on `visibility`, and
/// this closes the by-UUID detail/download path so a client can't fetch
/// internal work product (`review_memo`, `unclassified` lawyer/email
/// uploads) on their own matter by guessing or replaying a `doc_id`
/// (#782). The lawyer lens is unfiltered — lawyers see every asset.
///
/// `Ok(None)` is a genuine 404 (absent, cross-project, or internal under
/// the client lens); `Err` is a database failure the caller must surface
/// as a 500 rather than mask as not-found.
async fn load_doc_for_project(
    state: &AdminState,
    project_id: Uuid,
    doc_id: Uuid,
    lens: store::access::ProjectLens,
) -> Result<Option<store::assets::Asset>, store::assets::AssetError> {
    let Some(doc) = store::assets::find_by_id(&state.application_store, doc_id).await? else {
        tracing::info!(%project_id, %doc_id, "project document row not found");
        return Ok(None);
    };
    if doc.project_id != Some(project_id) {
        tracing::warn!(
            %project_id,
            %doc_id,
            doc_project_id = ?doc.project_id,
            "project document requested under the wrong project (cross-project guard)"
        );
        return Ok(None);
    }
    // A soft-deleted document is gone for a client; a lawyer keeps reading it
    // until the purge or a restore.
    if lens == store::access::ProjectLens::Client && doc.deleted_at.is_some() {
        tracing::info!(%project_id, %doc_id, "soft-deleted project document requested through the client lens");
        return Ok(None);
    }
    if lens == store::access::ProjectLens::Client
        && doc.visibility != store::documents::visibility::CLIENT
    {
        tracing::warn!(
            %project_id,
            %doc_id,
            visibility = %doc.visibility,
            "internal project document requested through the client lens (visibility guard)"
        );
        return Ok(None);
    }
    Ok(Some(doc))
}

/// Stream bytes through the app — fallback when the storage backend
/// has no signed-URL concept (`FsStorage` in local dev). Sets a
/// `Content-Disposition: attachment` so the browser downloads with the
/// original filename rather than an opaque storage key.
async fn stream_through(
    state: AdminState,
    key: &str,
    content_type: &str,
    filename: &str,
    disposition: Disposition,
) -> Response {
    match state.storage.get(key).await {
        Ok(obj) => bytes_response(obj.bytes, content_type, filename, disposition),
        Err(cloud::StorageError::NotFound(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, key, "storage get failed for project document");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Serve provider-named Inbox bytes with the document download safeguards.
pub(crate) fn attachment_bytes_response(
    bytes: Vec<u8>,
    content_type: &str,
    filename: &str,
) -> Response {
    let filename = filename
        .chars()
        .filter(|character| !character.is_control() && !matches!(character, '"' | '\\'))
        .collect::<String>();
    let filename = if filename.is_empty() {
        "attachment"
    } else {
        &filename
    };
    let mut response = bytes_response(bytes, content_type, filename, Disposition::Attachment);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}

fn bytes_response(
    bytes: Vec<u8>,
    content_type: &str,
    filename: &str,
    disposition: Disposition,
) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("{}; filename=\"{filename}\"", disposition.as_str()),
        )
        // The declared type is the caller's, so forbid the sniff that
        // would let a mislabelled body be re-read as something
        // executable.
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // The backstop behind the allowlist. `sandbox` with no tokens
        // drops the response into an opaque origin with scripting off,
        // so even a body that reaches `inline` under a wrong content
        // type cannot run as Navigator or read its cookies.
        .header(header::CONTENT_SECURITY_POLICY, "sandbox")
        .body(Body::from(bytes))
        .map_or_else(
            |e| {
                tracing::error!(error = %e, "build streaming response");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            },
            IntoResponse::into_response,
        )
}

#[cfg(test)]
mod tests {

    #[test]
    fn inbox_attachment_names_cannot_inject_headers_or_allow_shared_caching() {
        let response = super::attachment_bytes_response(
            Vec::new(),
            "text/html",
            "file\"\r\nX-Injected: yes\\.html",
        );
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert!(response.headers()[axum::http::header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment; filename=\""));
        assert!(!response.headers().contains_key("x-injected"));
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_SECURITY_POLICY],
            "sandbox"
        );
        assert_eq!(
            response.headers()[axum::http::header::CACHE_CONTROL],
            "private, no-store"
        );
    }
}
