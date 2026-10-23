//! Project document staging: upload bytes, retain only source-safe pointers.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use uuid::Uuid;

use crate::document_deletion::{
    deletion_prompt, record_actioned, short_commit, RemovedPointers, RETENTION_DAYS,
};
use crate::document_reconcile::{
    decide, Approval, Asker, Decision, Facts, LiveFact, PointerFact, Resolution, TerminalPrompter,
};
use crate::remote::DocumentClient;

/// The committed root of a Project repository's document pointers.
///
/// `blobs/<slug>.yaml` is the [Blob](../../docs/glossary/blob.md): YAML only,
/// tracked in Git, naming the live revision of the document at
/// `documents/<slug>`. The two roots mirror each other path for path.
pub(crate) const POINTER_DIRECTORY: &str = "blobs";

/// The wholly Git-ignored root a Project repository's document bytes are
/// staged into and pulled down to. The root `.gitignore` ignores all of it,
/// so no byte below it can be committed and no pointer is ever written here.
pub(crate) const DOCUMENT_DIRECTORY: &str = "documents";

/// The pointer `blobs/<slug>.yaml` for the bytes at `documents/<slug>`.
pub(crate) fn pointer_path_for_slug(root: &Path, slug: &Path) -> PathBuf {
    let mut path = root.join(POINTER_DIRECTORY).join(slug).into_os_string();
    path.push(format!(".{POINTER_EXTENSION}"));
    PathBuf::from(path)
}

/// The bytes `documents/<slug>` a pointer `blobs/<slug>.yaml` names.
pub(crate) fn document_path_for_slug(root: &Path, slug: &Path) -> PathBuf {
    root.join(DOCUMENT_DIRECTORY).join(slug)
}

/// The slug a pointer path below `blobs/` names, its pointer extension
/// stripped, or `None` when `relative` (root-relative) is not a pointer there.
pub(crate) fn slug_for_pointer(relative: &Path) -> Option<PathBuf> {
    let below = relative.strip_prefix(POINTER_DIRECTORY).ok()?;
    below
        .to_str()
        .and_then(strip_pointer_extension)
        .map(PathBuf::from)
}

/// The extension Navigator writes a document pointer with.
///
/// `.yaml`, matching every other YAML file Navigator owns in a Project
/// repository — `navigator.yaml`, `seeds/*.yaml`, `.sops.yaml`. Pointers were
/// the one exception (LAW-25), and the extension is in the CLI's own help
/// text, so it taught itself to every next repository.
///
/// `.github/workflows/*.yml` is GitHub's own convention and is not ours to
/// change.
pub(crate) const POINTER_EXTENSION: &str = "yaml";

/// Every extension a document pointer may be *read* at.
///
/// Writing one spelling while reading both is what lets a fleet-wide rename
/// happen after the release rather than atomically with it. The retired
/// `.yml` stays readable until no repository carries one.
///
/// This must be the only place the set is spelled. The walkers match on
/// extension, so a repository that renames ahead of the CLI reports
/// `0 pointer(s)` and passes — indistinguishable from having no documents at
/// all. That silent-pass failure is why the contract is central and not
/// per-command.
pub(crate) const POINTER_READ_EXTENSIONS: &[&str] = &[POINTER_EXTENSION, "yml"];

/// Whether `path` names a committed document pointer, in either spelling.
///
/// An [`AUTHORITY_SIDECAR_EXTENSION`] path is excluded even though its own
/// last extension is also `yaml`: a sidecar is pre-upload input a lawyer
/// wrote by hand, not a pointer `project sync` committed, and the two must never be
/// confused — `discover` sweeps the wrong one into the wrong list otherwise.
pub(crate) fn is_pointer_path(path: &Path) -> bool {
    if is_authority_sidecar_path(path) {
        return false;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| POINTER_READ_EXTENSIONS.contains(&extension))
}

/// The reserved sidecar extension for a staged `documents/cases/**` or
/// `documents/rules/**` capture: `<capture-filename>.authority.yaml`,
/// alongside the capture itself. Extends the same `<source>.<extension>`
/// naming [`is_pointer_path`] already reads pointers under, rather than
/// inventing a second scheme — the trailing `.yaml` is why [`is_pointer_path`]
/// must exclude it by name.
pub(crate) const AUTHORITY_SIDECAR_EXTENSION: &str = "authority.yaml";

/// Whether `path` names an Authority capture's sidecar.
pub(crate) fn is_authority_sidecar_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(&format!(".{AUTHORITY_SIDECAR_EXTENSION}")))
}

/// The sidecar path for a staged Authority capture at `path`, following the
/// same `<source>.<extension>` convention [`pointer_path_for_source`] writes
/// a pointer at.
fn authority_sidecar_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.{AUTHORITY_SIDECAR_EXTENSION}", path.display()))
}

/// The Authority folder `relative` (a staged path below `documents/`) is
/// captured under, or `None` when it names an ordinary Project document.
///
/// `documents/cases/**` and `documents/rules/**` are Authorities — global
/// reference data with no `project_id` (see the glossary's Authority entry)
/// — routed through `site authorities create` rather than filed as a Project
/// document. The two folders split [`rules::citation::AuthorityClass`]:
/// `cases/` holds case law, `rules/` holds everything else (statutes,
/// regulations, administrative proceedings, and secondary sources) —
/// [`authority_class_belongs_in_folder`] enforces the split against the
/// sidecar's own declared `class`.
fn authority_capture_folder(relative: &Path) -> Option<&'static str> {
    match relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
    {
        Some("cases") => Some("cases"),
        Some("rules") => Some("rules"),
        _ => None,
    }
}

/// Whether `class` (an [`AuthorityClass`](rules::citation::AuthorityClass)
/// wire value) belongs under `folder` — `"cases"` for `case_law` alone,
/// `"rules"` for every other class. A mismatch (a statute staged under
/// `cases/`, or case law staged under `rules/`) is refused by
/// [`read_authority_sidecar`] before either the network or
/// `documents/.gitignore` is touched, the same preflight discipline
/// [`validate_invoice_filename`] uses for `documents/invoices/`.
fn authority_class_belongs_in_folder(class: &str, folder: &str) -> bool {
    match folder {
        "cases" => class == "case_law",
        "rules" => class != "case_law",
        _ => false,
    }
}

/// Whether `relative` is a `documents/invoices/**` staged file.
fn is_invoice_capture(relative: &Path) -> bool {
    relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
        == Some("invoices")
}

/// The synthetic invoice-number filename shape this workspace's fixtures use:
/// `INV-` followed by one or more ASCII digits, then any extension (the
/// issue's own `INV-n.pdf` example). Real billing-system invoice numbering
/// is firm-confidential and not modeled here — this is a simple, documented
/// placeholder a synthetic fixture can satisfy, not an attempt to reproduce
/// a real numbering scheme.
fn is_invoice_filename(filename: &str) -> bool {
    let Some((stem, _extension)) = filename.rsplit_once('.') else {
        return false;
    };
    stem.strip_prefix("INV-").is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Refuse a `documents/invoices/**` file whose name does not match
/// [`is_invoice_filename`], per the issue's optional (but in-scope) folder
/// rule.
fn validate_invoice_filename(path: &Path) -> Result<()> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("{} has no filename", path.display()))?;
    if is_invoice_filename(filename) {
        Ok(())
    } else {
        Err(anyhow!(
            "{} does not match the invoice filename pattern `INV-<digits>.<ext>` (e.g. INV-1.pdf)",
            path.display()
        ))
    }
}

/// The two folders whose signed copy must be named `<name>_signed.pdf` and
/// carry `docusign_envelope_id` (ENG-859).
const SIGNED_ENGAGEMENT_FOLDERS: &[&str] = &["onboarding", "offboarding"];

/// Whether `relative` (a path relative to `documents/`) is staged under one
/// of [`SIGNED_ENGAGEMENT_FOLDERS`].
fn is_signed_engagement_folder(relative: &Path) -> bool {
    relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .is_some_and(|folder| SIGNED_ENGAGEMENT_FOLDERS.contains(&folder))
}

/// Whether `filename` carries the signed-copy suffix: `_signed.pdf` with a
/// non-empty name before it.
fn is_signed_pdf_filename(filename: &str) -> bool {
    filename
        .strip_suffix("_signed.pdf")
        .is_some_and(|stem| !stem.is_empty())
}

/// Enforce the `onboarding/`/`offboarding/`/`invoices/` pointer-key rules
/// (ENG-859). `relative` is the *source* document's path relative to
/// `documents/` — e.g. `onboarding/retainer_signed.pdf` — never its `.yaml`
/// pointer path.
///
/// `docusign_envelope_id` is the only structural signal a pointer carries
/// for "this is the signed copy", so the naming rule is enforced as a
/// biconditional under the two engagement folders: the id implies the
/// `_signed.pdf` name, and the name implies the id. Under `invoices/`,
/// `xero_invoice_id` is required unconditionally — every invoice pointer
/// names its Xero record, there is no unsigned-draft equivalent.
///
/// Only checks the id's *shape* (already enforced by `DocumentPointer`'s own
/// `Uuid` field type via `from_yaml`); resolving the id against a live
/// `DocuSign` envelope or Xero invoice is `project gate --check`'s separate
/// live-check pass (ENG-863), not this offline rule.
pub(crate) fn validate_folder_pointer_keys(
    relative: &Path,
    pointer: &store::document_pointers::DocumentPointer,
) -> Result<()> {
    if is_signed_engagement_folder(relative) {
        let filename = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let is_signed_name = is_signed_pdf_filename(filename);
        let has_envelope_id = pointer.docusign_envelope_id.is_some();
        if has_envelope_id && !is_signed_name {
            return Err(anyhow!(
                "{} carries docusign_envelope_id but is not named `<name>_signed.pdf`",
                relative.display()
            ));
        }
        if is_signed_name && !has_envelope_id {
            return Err(anyhow!(
                "{} is named `_signed.pdf` but its pointer has no docusign_envelope_id",
                relative.display()
            ));
        }
    }
    if is_invoice_capture(relative) && pointer.xero_invoice_id.is_none() {
        return Err(anyhow!(
            "{} is under invoices/ but its pointer has no xero_invoice_id",
            relative.display()
        ));
    }
    Ok(())
}

/// `slug` without its pointer extension, in either spelling, or `None` when
/// it names no pointer at all.
pub(crate) fn strip_pointer_extension(slug: &str) -> Option<&str> {
    POINTER_READ_EXTENSIONS
        .iter()
        .find_map(|extension| slug.strip_suffix(&format!(".{extension}")))
}

/// Read and validate `<root>/navigator.yaml` — the one manifest every
/// document command (`project sync`, and the read verbs under `navigator site document`)
/// resolves its Project and login host from. Returns `(project, host)`.
///
/// Routes through [`crate::projects::manifest::parse`], the one reader that
/// accepts both the current `project: {host, name}` shape and the deprecated
/// flat `project:`/`host:` shape, so a v2 manifest never fails with a
/// `serde_yaml` type-mismatch instead of this command's own error.
pub(crate) fn read_manifest(root: &Path) -> Result<(String, Option<String>)> {
    let manifest_path = root.join("navigator.yaml");
    let contents = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = crate::projects::manifest::parse(&contents).map_err(|error| anyhow!(error))?;
    let project = manifest
        .project
        .filter(|project| !project.trim().is_empty())
        .ok_or_else(|| anyhow!("navigator.yaml must name a Project"))?;
    Ok((project, manifest.host))
}

/// Resolve the pointer for one staged binary: the Authority route for
/// `documents/cases/**` and `documents/rules/**`, or the ordinary per-Project
/// document upload for everything else. `local_only_documents` already
/// reported any invoice filename that breaks the rule, so the ordinary route
/// never sees a rejected one.
#[allow(clippy::too_many_arguments)]
async fn resolve_pointer_for_binary(
    client: &DocumentClient,
    host: Option<&str>,
    path: &Path,
    bytes: &[u8],
    relative: &Path,
    slug: &str,
    existing_pointer: Option<&store::document_pointers::DocumentPointer>,
    dropped_into: Option<store::document_pointers::MatterFolder>,
) -> Result<store::document_pointers::DocumentPointer> {
    if let Some(folder) = authority_capture_folder(relative) {
        return sync_authority_capture(host, path, bytes, existing_pointer, folder).await;
    }
    // A new file takes the kind its slug names, else its matter folder's.
    let kind = existing_pointer.map_or_else(
        || match (inferred_kind(Path::new(slug)), dropped_into) {
            ("unclassified", Some(folder)) => folder.default_kind(),
            (kind, _) => kind,
        },
        |pointer| pointer.kind.as_str(),
    );
    let desired_visibility = existing_pointer.map_or_else(
        || "internal".to_string(),
        |pointer| pointer.visibility.clone(),
    );
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("file path has no filename"))?;
    let uploaded_pointer = client
        .upload_bytes(
            filename,
            bytes,
            kind,
            Some(&desired_visibility),
            None,
            Some(content_type(path)),
            Some(slug),
            None,
        )
        .await?;
    validate_upload_receipt(&uploaded_pointer, bytes)?;
    Ok(uploaded_pointer)
}

/// Write one resolved pointer for `path`, restoring the previous pointer if
/// the source changed underneath the upload. The bytes stay where they are.
fn commit_pointer_for_source(
    root: &Path,
    path: &Path,
    pointer_path: &Path,
    pointer: &store::document_pointers::DocumentPointer,
    bytes: &[u8],
    existing_pointer_raw: Option<&str>,
    has_existing_pointer: bool,
) -> Result<()> {
    ensure_source_is_unchanged(path, bytes)?;
    let pointer_yaml = pointer.to_yaml()?;
    prepare_pointer_parent(root, pointer_path)?;
    ensure_document_path_is_safe(root, pointer_path, !has_existing_pointer, false)?;
    write_pointer_atomically(pointer_path, &pointer_yaml)?;
    if let Err(error) = ensure_source_is_unchanged(path, bytes) {
        restore_pointer_after_source_change(
            pointer_path,
            existing_pointer_raw,
            has_existing_pointer,
        )
        .map_err(|restore| anyhow!("{error:#}; restore pointer: {restore:#}"))?;
        return Err(error);
    }
    Ok(())
}

/// The Authority metadata a staged `documents/cases/**` or
/// `documents/rules/**` capture must supply via its sidecar
/// (`<capture>.authority.yaml`) — exactly the fields `site authorities
/// create` needs and cannot reliably scrape from arbitrary HTML `<meta>`
/// tags (settled in the issue's own triage comment).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoritySidecar {
    class: String,
    citation: String,
    title: String,
    #[serde(default)]
    short_cite: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default)]
    issued_on: Option<String>,
    #[serde(default)]
    canonical_url: Option<String>,
    #[serde(default)]
    checked_on: Option<String>,
}

/// Read and parse one staged Authority capture's sidecar
/// (`<capture>.authority.yaml`), and enforce that its declared `class`
/// belongs under `folder` ([`authority_class_belongs_in_folder`]) — shared by
/// [`local_only_documents`], which reports a missing, malformed, or misfiled
/// sidecar instead of filing the capture, and by [`sync_authority_capture`],
/// which reads the same fields it validated.
fn read_authority_sidecar(path: &Path, folder: &str) -> Result<AuthoritySidecar> {
    let sidecar_path = authority_sidecar_path(path);
    let sidecar_raw = std::fs::read_to_string(&sidecar_path).with_context(|| {
        format!(
            "read {} (a documents/{folder}/ capture needs a sidecar carrying citation/class/title)",
            sidecar_path.display()
        )
    })?;
    let sidecar: AuthoritySidecar = serde_yaml::from_str(&sidecar_raw)
        .with_context(|| format!("parse {}", sidecar_path.display()))?;
    if !authority_class_belongs_in_folder(&sidecar.class, folder) {
        let other = if folder == "cases" { "rules" } else { "cases" };
        return Err(anyhow!(
            "{} declares class `{}`, which belongs under documents/{other}/, not documents/{folder}/",
            sidecar_path.display(),
            sidecar.class
        ));
    }
    Ok(sidecar)
}

/// Route one staged `documents/cases/**` or `documents/rules/**` capture
/// through `site authorities create` instead of the ordinary per-Project
/// document upload — Authorities are global reference data with no
/// `project_id` (see the glossary's Authority entry), so filing one as a
/// Project document is the misclassification this routing exists to avoid.
///
/// Reads the capture's sidecar, archives the bytes through the same
/// authenticated door `navigator site authorities create` uses, and returns
/// a [`store::document_pointers::DocumentPointer`] carrying the resulting
/// `authority_id` alongside `sha256`/`canonical_url`/`checked_on`/`created_at`
/// — reusing the existing pointer struct rather than a parallel shape.
/// `kind` is empty (LAW-82): the capture is archived under a global
/// Authority, never filed on this Project as a document, so no asset-lane
/// kind describes it and `project sync`/`gate --check` must not treat it as one.
async fn sync_authority_capture(
    host: Option<&str>,
    path: &Path,
    bytes: &[u8],
    existing_pointer: Option<&store::document_pointers::DocumentPointer>,
    folder: &str,
) -> Result<store::document_pointers::DocumentPointer> {
    let sidecar = read_authority_sidecar(path, folder)?;
    let authority = crate::authorities::create_authority(
        host,
        &crate::authorities::NewAuthorityArgs {
            class: &sidecar.class,
            citation: &sidecar.citation,
            title: &sidecar.title,
            short_cite: sidecar.short_cite.as_deref(),
            publisher: sidecar.publisher.as_deref(),
            issued_on: sidecar.issued_on.as_deref(),
            canonical_url: sidecar.canonical_url.as_deref(),
            checked_on: sidecar.checked_on.as_deref(),
            file: path,
            content_type: Some(content_type(path)),
        },
    )
    .await
    .with_context(|| format!("archive {} as an Authority", path.display()))?;
    let asset_id = authority
        .archived_asset_id
        .ok_or_else(|| anyhow!("authority create archived no asset for {}", path.display()))?;
    let version = existing_pointer.map_or(1, |pointer| pointer.current_version.version + 1);
    let previous_version = existing_pointer.map(|pointer| pointer.current_version.asset_id);
    let pointer = store::document_pointers::DocumentPointer {
        kind: String::new(),
        visibility: "internal".to_string(),
        folder: None,
        current_version: store::document_pointers::PointerVersion {
            version,
            asset_id,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            sha256: store::documents::sha256_hex(bytes),
            size_bytes: i64::try_from(bytes.len())
                .context("authority capture byte count does not fit in pointer")?,
            canonical_url: sidecar.canonical_url,
            checked_on: sidecar.checked_on,
        },
        previous_version,
        authority_id: Some(authority.id),
        // An Authority capture is never a signed engagement letter or an
        // invoice — those two keys are always absent here.
        docusign_envelope_id: None,
        xero_invoice_id: None,
        signing: None,
        notarization: Vec::new(),
        service: Vec::new(),
    };
    pointer
        .validate()
        .map_err(|error| anyhow!("invalid authority pointer: {error}"))?;
    Ok(pointer)
}

/// Reconcile the complete caller-visible live document inventory into the
/// current Project checkout, including documents whose pointer has never
/// existed locally.
pub(crate) async fn run_project_sync(
    root: &Path,
    dry_run: bool,
    yes: bool,
    yes_delete: bool,
    only: Option<&Path>,
) -> ExitCode {
    match project_sync(root, dry_run, yes, yes_delete, only).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// One slug path or directory below `documents/` (or its Blob below
/// `blobs/`). Component matching keeps
/// `cases` distinct from `cases-old` and never widens an explicit scope.
#[derive(Debug, Clone)]
pub(crate) struct DocumentScope(Option<PathBuf>);

impl DocumentScope {
    #[cfg(test)]
    pub(crate) fn all() -> Self {
        Self(None)
    }
    fn parse(root: &Path, only: Option<&Path>) -> Result<Self> {
        let Some(path) = only else {
            return Ok(Self(None));
        };
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(anyhow!(
                "--only must name a path below documents/ or blobs/ without traversal"
            ));
        }
        let relative = path
            .strip_prefix(DOCUMENT_DIRECTORY)
            .or_else(|_| path.strip_prefix(POINTER_DIRECTORY))
            .map_err(|_| anyhow!("--only must name a path below documents/ or blobs/"))?;
        if relative.as_os_str().is_empty() {
            return Ok(Self(None));
        }
        let mut current = root.to_path_buf();
        for component in path.components() {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata_is_symlink(&metadata) => {
                    return Err(anyhow!(
                        "refusing to scope through symlink {}",
                        current.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(error).with_context(|| format!("inspect {}", current.display()))
                }
            }
        }
        // Bytes in a matter folder name the slug below it (LAW-189).
        let source = match relative.to_str().and_then(strip_pointer_extension) {
            Some(slug) => PathBuf::from(slug),
            None if path.starts_with(DOCUMENT_DIRECTORY) => {
                crate::document_layout::slug_of_bytes(relative).0
            }
            None => relative.to_path_buf(),
        };
        if source.as_os_str().is_empty() {
            return Err(anyhow!(
                "--only must name a document or folder below a matter folder, not the whole folder"
            ));
        }
        Ok(Self(Some(source)))
    }

    fn includes(&self, relative: &Path) -> bool {
        self.0
            .as_ref()
            .is_none_or(|selected| relative.starts_with(selected))
    }
}

/// The local bytes of one document, as the sync saw them.
#[derive(Debug)]
struct LocalCopy {
    sha256: String,
    size: usize,
}

/// One live document set against its local bytes and its committed pointer.
#[derive(Debug)]
struct ProjectSyncPlan {
    label: String,
    /// The document identity, `None` for a live row that has no slug (and so
    /// cannot take a new revision).
    slug: Option<String>,
    /// The pointer the live row describes.
    pointer: store::document_pointers::DocumentPointer,
    /// The pointer committed in the checkout, if any.
    committed: Option<store::document_pointers::DocumentPointer>,
    pointer_target: PathBuf,
    document_target: PathBuf,
    pointer_changed: bool,
    local: Option<LocalCopy>,
    /// The newest live revision's number (the chain length) and size.
    live_revision: usize,
    live_size: i64,
    decision: Decision,
    /// The commit that removed this document's pointer, when it did.
    removed_in: Option<String>,
    /// Keys the committed pointer records that the live revision does not
    /// carry: the live row names new bytes, so the pointer is rewritten
    /// without them. Reported, never dropped silently.
    dropped_keys: Vec<&'static str>,
}

/// The execution and external-id keys a pointer records by hand or from an
/// integration, which the live inventory does not model.
fn recorded_external_keys(
    pointer: &store::document_pointers::DocumentPointer,
) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if pointer.docusign_envelope_id.is_some() {
        keys.push("docusign_envelope_id");
    }
    if pointer.xero_invoice_id.is_some() {
        keys.push("xero_invoice_id");
    }
    if pointer.signing.is_some() {
        keys.push("signing");
    }
    if !pointer.notarization.is_empty() {
        keys.push("notarization");
    }
    if !pointer.service.is_empty() {
        keys.push("service");
    }
    keys
}

/// Carry what a committed pointer records by hand or from an integration onto
/// the pointer an upload just produced: the server models none of the two ids,
/// and a signing or notarization block stays unless the live row now has one.
fn carry_recorded_keys(
    pointer: &mut store::document_pointers::DocumentPointer,
    from: &store::document_pointers::DocumentPointer,
) {
    pointer.folder = from.folder;
    pointer.docusign_envelope_id = from.docusign_envelope_id;
    pointer.xero_invoice_id = from.xero_invoice_id;
    pointer.signing = pointer.signing.or(from.signing);
    if pointer.notarization.is_empty() {
        pointer.notarization.clone_from(&from.notarization);
    }
    if pointer.service.is_empty() {
        pointer.service.clone_from(&from.service);
    }
}

fn dropped_keys_note(plan: &ProjectSyncPlan) -> String {
    format!(
        "{}: the new live revision does not carry {}; the rewritten pointer omits {}",
        plan.label,
        plan.dropped_keys.join(", "),
        if plan.dropped_keys.len() == 1 {
            "it"
        } else {
            "them"
        }
    )
}

/// The local file at `path`, hashed, or `None` when it does not exist.
fn read_local_copy(path: &Path) -> Result<Option<LocalCopy>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(LocalCopy {
            sha256: store::documents::sha256_hex(&bytes),
            size: bytes.len(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn inventory_filename(document: &crate::remote::LiveDocumentSummary) -> Result<String> {
    let filename = document
        .filename
        .as_deref()
        .filter(|name| {
            let path = Path::new(name);
            path.components().count() == 1 && path.file_name().is_some()
        })
        .ok_or_else(|| anyhow!("document {} has no safe filename", document.id))?;
    Ok(filename.to_string())
}

fn inventory_relative_path(document: &crate::remote::LiveDocumentSummary) -> Result<PathBuf> {
    if let Some(slug) = document.slug.as_deref() {
        let relative = PathBuf::from(slug);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || relative.file_name().is_none()
        {
            return Err(anyhow!("document {} has unsafe slug `{slug}`", document.id));
        }
        return Ok(relative);
    }
    Ok(PathBuf::from("unclassified")
        .join(document.id.to_string())
        .join(inventory_filename(document)?))
}

fn pointer_target_for_document(root: &Path, relative: &Path) -> Result<PathBuf> {
    let target = pointer_path_for_slug(root, relative);
    let retired = target.with_extension("yml");
    if retired.exists() {
        return Err(anyhow!(
            "{} uses a retired pointer suffix; rename it to {} before syncing",
            retired.display(),
            target.display()
        ));
    }
    Ok(target)
}

pub(crate) fn write_uploaded_pointer(
    root: &Path,
    slug: &str,
    pointer: &store::document_pointers::DocumentPointer,
) -> Result<PathBuf> {
    let relative = PathBuf::from(slug);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || relative.file_name().is_none()
    {
        return Err(anyhow!("document has unsafe slug `{slug}`"));
    }
    let blobs = root.join(POINTER_DIRECTORY);
    ensure_documents_tree_is_safe(&blobs)?;
    let path = pointer_target_for_document(root, &relative)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    ensure_documents_tree_is_safe(&blobs)?;
    ensure_document_path_is_safe(root, &path, true, false)?;
    write_pointer_atomically(&path, &pointer.to_yaml()?)?;
    Ok(path)
}

fn raw_document_is_git_ignored(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| anyhow!("{} is outside {}", target.display(), root.display()))?;
    let status = std::process::Command::new("git")
        .args(["check-ignore", "--quiet", "--"])
        .arg(relative)
        .current_dir(root)
        .status()
        .with_context(|| format!("verify Git ignores {}", relative.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "refusing to download {} because Git does not ignore it",
            relative.display()
        ))
    }
}

/// A raw copy `target` below `root/documents` may be (re)written only when no
/// component is a symlink and Git ignores it.
pub(crate) fn ensure_managed_copy_target(root: &Path, target: &Path) -> Result<()> {
    ensure_document_path_is_safe(root, target, true, false)?;
    raw_document_is_git_ignored(root, target)
}

fn current_pointer_matches(
    path: &Path,
    pointer: &store::document_pointers::DocumentPointer,
) -> Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(store::document_pointers::DocumentPointer::from_yaml(&raw)? == *pointer),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// The path of every committed pointer, by the asset id its current revision
/// names. Read only when a pointer looks removed, to tell a move from a
/// deletion.
fn local_pointer_assets(root: &Path) -> Result<BTreeMap<Uuid, String>> {
    let mut homes = BTreeMap::new();
    for relative in crate::document_read::discover_pointers(root)? {
        if let Some(pointer) = read_pointer(&root.join(&relative))? {
            homes.insert(
                pointer.current_version.asset_id,
                display_relative(root, &root.join(&relative)),
            );
        }
    }
    Ok(homes)
}

/// The live Service records of a Project, by the served asset id.
pub(crate) type LiveService = BTreeMap<Uuid, Vec<store::project_service::ServiceRecord>>;

#[allow(clippy::too_many_lines)]
fn project_sync_plans_scoped(
    root: &Path,
    documents: Vec<crate::remote::LiveDocumentSummary>,
    removed: &RemovedPointers,
    scope: &DocumentScope,
    live_service: &LiveService,
) -> Result<Vec<ProjectSyncPlan>> {
    let mut pointer_homes: Option<BTreeMap<Uuid, String>> = None;
    let mut grouped: BTreeMap<String, Vec<crate::remote::LiveDocumentSummary>> = BTreeMap::new();
    for document in documents {
        let key = document
            .slug
            .clone()
            .unwrap_or_else(|| format!("@{}", document.id));
        grouped.entry(key).or_default().push(document);
    }
    let mut plans = Vec::with_capacity(grouped.len());
    for (_, revisions) in grouped {
        let current = revisions
            .first()
            .ok_or_else(|| anyhow!("empty document revision group"))?;
        let relative = inventory_relative_path(current)?;
        if !scope.includes(&relative) {
            continue;
        }
        let pointer_target = pointer_target_for_document(root, &relative)?;
        let kind = current
            .kind
            .clone()
            .unwrap_or_else(|| "unclassified".to_string());
        // The live inventory carries neither external-id key, so they come
        // from the committed pointer — but only while it still names this
        // revision; a new revision has not been signed or invoiced yet.
        let committed = read_pointer(&pointer_target)?;
        // The same bytes under a new revision id (a re-upload, a re-slug) are
        // still the signed or invoiced instrument; different bytes are not.
        let local = committed.clone().filter(|local| {
            local.current_version.asset_id == current.id
                || local.current_version.sha256 == current.sha256_hex
        });
        let dropped_keys = match (&committed, &local) {
            (Some(committed), None) => recorded_external_keys(committed),
            _ => Vec::new(),
        };
        let pointer = store::document_pointers::DocumentPointer {
            kind,
            visibility: current.visibility.clone(),
            // The live inventory has no folder; the committed pointer names it.
            folder: committed.as_ref().and_then(|committed| committed.folder),
            current_version: store::document_pointers::PointerVersion {
                version: revisions.len(),
                asset_id: current.id,
                created_at: normalize_created_at(&current.inserted_at),
                sha256: current.sha256_hex.clone(),
                size_bytes: current.byte_size,
                canonical_url: None,
                checked_on: None,
            },
            previous_version: revisions.get(1).map(|revision| revision.id),
            authority_id: None,
            // ENG-859: `projects sync` does not resolve either external-id
            // key from a live DocuSign/Xero link yet.
            docusign_envelope_id: local.as_ref().and_then(|local| local.docusign_envelope_id),
            xero_invoice_id: local.as_ref().and_then(|local| local.xero_invoice_id),
            // The live record wins; a block recorded by hand on the committed
            // pointer is kept while the live row has none.
            signing: current
                .signing
                .or_else(|| local.as_ref().and_then(|local| local.signing)),
            notarization: if current.notarization.is_empty() {
                local
                    .as_ref()
                    .map(|local| local.notarization.clone())
                    .unwrap_or_default()
            } else {
                current.notarization.clone()
            },
            // Every record on any revision of the chain: a served copy stays
            // served when the document takes a new revision. A hand-recorded
            // list is kept while the live Project has none.
            service: {
                let mut live: Vec<store::project_service::ServiceRecord> = revisions
                    .iter()
                    .filter_map(|revision| live_service.get(&revision.id))
                    .flatten()
                    .cloned()
                    .collect();
                live.sort_by(|a, b| (a.served_on, &a.recipient).cmp(&(b.served_on, &b.recipient)));
                if live.is_empty() {
                    local
                        .as_ref()
                        .map(|local| local.service.clone())
                        .unwrap_or_default()
                } else {
                    live
                }
            },
        };
        pointer
            .validate()
            .map_err(|error| anyhow!("invalid live pointer for {}: {error}", relative.display()))?;
        let pointer_changed = !current_pointer_matches(&pointer_target, &pointer)?;
        let document_target = crate::document_layout::document_path(root, &relative, &pointer);
        let local_copy = read_local_copy(&document_target)?;
        let chain: Vec<Uuid> = revisions.iter().map(|revision| revision.id).collect();
        // A repository that removed this document's pointer is asking to
        // delete it. A closed_repository archive is never deleted by sync, and
        // a pointer that merely moved (an asset id another pointer still
        // names) is not a removal. A chain with no pointer history is not a
        // sync deletion; retire that mistaken chain with `site document retire`.
        let removed_in = match (&committed, current.slug.as_deref()) {
            (None, Some(slug)) if current.kind.as_deref() != Some("closed_repository") => {
                removed.pending_removal(slug).map(str::to_string)
            }
            _ => None,
        };
        let moved_to = if removed_in.is_some() {
            let homes = match &mut pointer_homes {
                Some(homes) => homes,
                slot => slot.insert(local_pointer_assets(root)?),
            };
            chain.iter().find_map(|id| homes.get(id).cloned())
        } else {
            None
        };
        let mut decision = decide(&Facts {
            removed_in: removed_in.as_deref().filter(|_| moved_to.is_none()),
            local: local_copy.as_ref().map(|copy| copy.sha256.as_str()),
            pointer: committed.as_ref().map(|committed| PointerFact {
                asset_id: committed.current_version.asset_id,
                sha256: &committed.current_version.sha256,
            }),
            live: Some(LiveFact {
                operative: current.id,
                sha256: &current.sha256_hex,
                chain: &chain,
            }),
        });
        if decision == Decision::Nothing && pointer_changed {
            decision = Decision::RewritePointer;
        }
        if moved_to.is_some() {
            decision = Decision::CouldNotReconcile(
                "its pointer moved to another path; use `navigator site document reslug` to move the live document too",
            );
        }
        // A live row with no slug has no identity a new revision could join.
        if current.slug.is_none() && decision == Decision::Upload {
            decision = Decision::CouldNotReconcile(
                "the live row has no slug, so local edits cannot be uploaded as a revision",
            );
        }
        plans.push(ProjectSyncPlan {
            label: format!("documents/{}", slash_path(&relative)?),
            slug: current.slug.clone(),
            pointer,
            committed,
            pointer_target,
            document_target,
            pointer_changed,
            local: local_copy,
            live_revision: revisions.len(),
            live_size: current.byte_size,
            decision,
            removed_in,
            dropped_keys,
        });
    }
    Ok(plans)
}

#[cfg(test)]
fn project_sync_plans(
    root: &Path,
    documents: Vec<crate::remote::LiveDocumentSummary>,
    removed: &RemovedPointers,
) -> Result<Vec<ProjectSyncPlan>> {
    project_sync_plans_scoped(
        root,
        documents,
        removed,
        &DocumentScope(None),
        &LiveService::new(),
    )
}

/// One Authority capture's raw copy: a committed pointer under
/// `documents/cases` or `documents/rules` carrying an `authority_id`, whose
/// bytes live only in the Authority archive.
struct AuthorityCopy {
    label: String,
    authority_id: Uuid,
    target: PathBuf,
    sha256: String,
    size_bytes: i64,
    missing: bool,
}

/// The Authority copies a checkout's committed pointers name. Authority
/// pointers are not Project documents, so the inventory never lists them.
fn authority_copies(root: &Path, scope: &DocumentScope) -> Result<Vec<AuthorityCopy>> {
    let mut copies = Vec::new();
    for relative in crate::document_read::discover_pointers(root)? {
        let Some(slug) = slug_for_pointer(&relative) else {
            continue;
        };
        if !scope.includes(&slug) || authority_capture_folder(&slug).is_none() {
            continue;
        }
        let Some(pointer) = read_pointer(&root.join(&relative))? else {
            continue;
        };
        let Some(authority_id) = pointer.authority_id else {
            continue;
        };
        let target = pull_target(root, &relative)?;
        let missing = !matches_digest(&target, &pointer.current_version.sha256)?;
        copies.push(AuthorityCopy {
            label: display_relative(root, &target),
            authority_id,
            target,
            sha256: pointer.current_version.sha256,
            size_bytes: pointer.current_version.size_bytes,
            missing,
        });
    }
    Ok(copies)
}

/// How one plan resolved once its question, if it had one, was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Nothing,
    /// Write the pointer; the bytes already match.
    Pointer,
    /// Write the pointer and download the live revision.
    Fetch,
    /// Upload the local bytes as a new revision.
    Upload,
    /// Soft-delete the live document: its pointer was removed.
    Delete,
    Declined,
    /// No terminal and no `--yes`: the item was left as it was.
    Refused,
    /// Reported only; neither side is touched.
    Unreconciled,
}

fn short(sha256: &str) -> &str {
    &sha256[..sha256.len().min(8)]
}

/// The facts a person needs to answer about one document: its slug, both
/// sha256 prefixes, the sizes, and the revision numbers.
fn evidence(plan: &ProjectSyncPlan) -> String {
    let mut lines = vec![plan.label.clone()];
    lines.push(plan.local.as_ref().map_or_else(
        || "  local    absent".to_string(),
        |local| {
            format!(
                "  local    sha256 {}  {} bytes",
                short(&local.sha256),
                local.size
            )
        },
    ));
    if let Some(committed) = &plan.committed {
        lines.push(format!(
            "  pointer  revision {}  sha256 {}  {} bytes",
            committed.current_version.version,
            short(&committed.current_version.sha256),
            committed.current_version.size_bytes
        ));
    }
    lines.push(format!(
        "  live     revision {}  sha256 {}  {} bytes",
        plan.live_revision,
        short(&plan.pointer.current_version.sha256),
        plan.live_size
    ));
    if !plan.dropped_keys.is_empty() {
        lines.push(format!("  {}", dropped_keys_note(plan)));
    }
    lines.join("\n")
}

fn replace_prompt(plan: &ProjectSyncPlan) -> String {
    format!(
        "{}\nOverwrite the local copy with live revision {}?",
        evidence(plan),
        plan.live_revision
    )
}

fn upload_prompt(plan: &ProjectSyncPlan) -> String {
    format!(
        "{}\nUpload the local bytes as a new revision of `{}`?",
        evidence(plan),
        plan.slug.as_deref().unwrap_or(&plan.label)
    )
}

fn conflict_prompt(plan: &ProjectSyncPlan) -> String {
    format!(
        "{}\nConflict: the local bytes were edited and live revision {} is newer. Local wins uploads them as \
         revision {}; live wins overwrites your local edits. Nothing is merged.",
        evidence(plan),
        plan.live_revision,
        plan.live_revision + 1
    )
}

fn answered(resolution: Resolution, applied: Step) -> Step {
    match resolution {
        Resolution::Apply | Resolution::ApplyLocal => applied,
        Resolution::Decline => Step::Declined,
        Resolution::Refused => Step::Refused,
    }
}

fn resolve_plan(plan: &ProjectSyncPlan, asker: &mut Asker, yes_delete: bool) -> Result<Step> {
    Ok(match plan.decision {
        // A live row is never a new file; `local_only_documents` owns those.
        Decision::Nothing | Decision::UploadNew => Step::Nothing,
        Decision::RewritePointer => Step::Pointer,
        Decision::Download => Step::Fetch,
        Decision::ReplaceLocal => answered(asker.confirm(&replace_prompt(plan))?, Step::Fetch),
        Decision::Upload => answered(asker.confirm(&upload_prompt(plan))?, Step::Upload),
        Decision::Conflict => {
            match asker.choose_side(&conflict_prompt(plan), plan.slug.is_some())? {
                Resolution::ApplyLocal => Step::Upload,
                other => answered(other, Step::Fetch),
            }
        }
        Decision::SoftDelete => answered(
            asker.confirm_deletion(&delete_prompt(plan), yes_delete)?,
            Step::Delete,
        ),
        Decision::CouldNotReconcile(_) => Step::Unreconciled,
    })
}

/// The date a soft deletion made today stops being restorable.
fn purge_date() -> chrono::NaiveDate {
    (chrono::Utc::now() + chrono::Duration::days(RETENTION_DAYS)).date_naive()
}

fn delete_prompt(plan: &ProjectSyncPlan) -> String {
    deletion_prompt(
        &plan.label,
        plan.slug.as_deref().unwrap_or(&plan.label),
        &plan.pointer.kind,
        &plan.pointer.visibility,
        plan.live_revision,
        plan.removed_in.as_deref().unwrap_or_default(),
        purge_date(),
    )
}

/// The line a dry run prints for one plan.
fn dry_run_line(plan: &ProjectSyncPlan) -> String {
    let slug = plan.slug.as_deref().unwrap_or(&plan.label);
    match plan.decision {
        Decision::Nothing | Decision::UploadNew => format!("skip {}", plan.label),
        Decision::RewritePointer => format!("would rewrite pointer {}", plan.label),
        Decision::Download if !plan.pointer_target.exists() => {
            format!("would add {}", plan.label)
        }
        Decision::Download => format!("would download {}", plan.label),
        Decision::ReplaceLocal => format!(
            "would ask to overwrite the local copy with live revision {}: {}",
            plan.live_revision, plan.label
        ),
        Decision::Upload => format!(
            "would ask to upload local edits as a new revision of {slug}: {}",
            plan.label
        ),
        Decision::Conflict => format!(
            "conflict {}: would ask which side wins (local edits are overwritten if live wins)",
            plan.label
        ),
        Decision::SoftDelete => format!(
            "would ask to soft-delete {} (its pointer was removed in {}; recoverable for {RETENTION_DAYS} days)",
            plan.label,
            short_commit(plan.removed_in.as_deref().unwrap_or_default())
        ),
        Decision::CouldNotReconcile(reason) => format!("cannot reconcile {}: {reason}", plan.label),
    }
}

/// A file under `documents/` that the live Project has no document for.
struct NewFile {
    path: PathBuf,
    label: String,
}

/// What lives only in the checkout: new files, and pointers whose document
/// the live Project does not list.
#[derive(Default)]
struct LocalOnly {
    new_files: Vec<NewFile>,
    orphan_pointers: Vec<String>,
    notices: Vec<String>,
}

fn local_only_documents(
    root: &Path,
    live: &std::collections::BTreeSet<String>,
    removed: &RemovedPointers,
    scope: &DocumentScope,
) -> Result<LocalOnly> {
    let documents = root.join(DOCUMENT_DIRECTORY);
    let binaries = discover(&documents)?;
    let mut local = LocalOnly::default();
    for path in binaries {
        ensure_document_path_is_safe(root, &path, false, true)?;
        let relative = path
            .strip_prefix(&documents)
            .map_err(|_| anyhow!("{} is outside documents/", path.display()))?;
        // Bytes in a matter folder name the slug below it (LAW-189).
        let (slug_path, _) = crate::document_layout::slug_of_bytes(relative);
        if !scope.includes(&slug_path) {
            continue;
        }
        let slug = slash_path(&slug_path)?;
        if live.contains(&slug) || pointer_path_for_source(root, &path)?.1 {
            continue;
        }
        // An Authority capture needs its sidecar; without one it is reported,
        // never filed as a Project document.
        if let Some(folder) = authority_capture_folder(relative) {
            if let Err(error) = read_authority_sidecar(&path, folder) {
                local.notices.push(format!("documents/{slug}: {error:#}"));
                continue;
            }
        }
        // Bytes left behind by a deleted pointer are not a new document.
        if let Some(commit) = removed.last_removal(&slug) {
            local.notices.push(format!(
                "documents/{slug}: its pointer was removed in {}; delete the local copy, or restore the pointer to keep the document",
                short_commit(commit)
            ));
            continue;
        }
        if is_invoice_capture(relative) {
            if let Err(error) = validate_invoice_filename(&path) {
                local.notices.push(format!("{error:#}"));
                continue;
            }
        }
        local.new_files.push(NewFile {
            label: format!("documents/{}", slash_path(relative)?),
            path,
        });
    }
    for pointer in crate::document_read::discover_pointers(root)? {
        let Some(slug) = slug_for_pointer(&pointer) else {
            continue;
        };
        if !scope.includes(&slug) || authority_capture_folder(&slug).is_some() {
            continue;
        }
        let slug = slash_path(&slug)?;
        if !live.contains(&slug) {
            local.orphan_pointers.push(slash_path(&pointer)?);
        }
    }
    Ok(local)
}

async fn stage_project_sync(
    items: &[(&ProjectSyncPlan, bool)],
    authorities: &[AuthorityCopy],
    client: &DocumentClient,
    staging: &tempfile::TempDir,
) -> Result<(Vec<StagedPull>, Vec<String>)> {
    let downloads = staging.path().join("downloads");
    std::fs::create_dir_all(&downloads).context("create project sync downloads")?;
    let mut staged = Vec::new();
    let mut unreadable = Vec::new();
    for (plan, fetch) in items {
        let mut bytes_path = None;
        if *fetch {
            // A 401, a 403, or any other refusal is "could not read", never
            // "absent": the item is skipped and nothing is written over it.
            match client
                .download_revision(plan.pointer.current_version.asset_id)
                .await
            {
                Ok(bytes)
                    if store::documents::sha256_hex(&bytes)
                        == plan.pointer.current_version.sha256 =>
                {
                    let staged_path = downloads.join(staged.len().to_string());
                    std::fs::write(&staged_path, bytes)?;
                    bytes_path = Some(staged_path);
                }
                Ok(bytes) => {
                    unreadable.push(format!(
                        "{}: could not read: sha256 mismatch (expected {}, received {})",
                        plan.label,
                        plan.pointer.current_version.sha256,
                        store::documents::sha256_hex(&bytes)
                    ));
                    continue;
                }
                Err(error) => {
                    unreadable.push(format!("{}: could not read: {error:#}", plan.label));
                    continue;
                }
            }
        }
        if plan.pointer_changed {
            let staged_path = downloads.join(format!("pointer-{}", staged.len()));
            std::fs::write(&staged_path, plan.pointer.to_yaml()?)?;
            staged.push(StagedPull {
                target: plan.pointer_target.clone(),
                staged: staged_path,
            });
        }
        if let Some(staged_path) = bytes_path {
            staged.push(StagedPull {
                target: plan.document_target.clone(),
                staged: staged_path,
            });
        }
    }
    for copy in authorities.iter().filter(|copy| copy.missing) {
        match client.download_authority_archive(copy.authority_id).await {
            Ok(bytes) => {
                let actual = store::documents::sha256_hex(&bytes);
                if actual != copy.sha256 {
                    unreadable.push(format!(
                        "{}: could not read: sha256 mismatch (expected {}, received {actual})",
                        copy.label, copy.sha256
                    ));
                } else if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != copy.size_bytes {
                    unreadable.push(format!(
                        "{}: could not read: size mismatch (expected {} bytes, received {})",
                        copy.label,
                        copy.size_bytes,
                        bytes.len()
                    ));
                } else {
                    let staged_path = downloads.join(staged.len().to_string());
                    std::fs::write(&staged_path, bytes)?;
                    staged.push(StagedPull {
                        target: copy.target.clone(),
                        staged: staged_path,
                    });
                }
            }
            Err(error) => unreadable.push(format!("{}: could not read: {error:#}", copy.label)),
        }
    }
    Ok((staged, unreadable))
}

/// Upload one local file as a new revision of its slug (or as a new
/// document), write the resulting pointer, and keep the local bytes: the
/// matching sha256 is what makes the next run a no-op.
async fn upload_local_revision(
    client: &DocumentClient,
    root: &Path,
    path: &Path,
    base: Option<&store::document_pointers::DocumentPointer>,
    carry: Option<&store::document_pointers::DocumentPointer>,
) -> Result<()> {
    ensure_document_path_is_safe(root, path, false, true)?;
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let relative = path
        .strip_prefix(root.join(DOCUMENT_DIRECTORY))
        .map_err(|_| anyhow!("{} is outside documents/", path.display()))?;
    // Bytes in a matter folder name the slug below it, and that folder is
    // where they stay (LAW-189).
    let (slug_path, dropped_into) = crate::document_layout::slug_of_bytes(relative);
    let slug = slash_path(&slug_path)?;
    let (pointer_path, has_existing_pointer) = pointer_path_for_source(root, path)?;
    prepare_pointer_parent(root, &pointer_path)?;
    ensure_document_path_is_safe(root, &pointer_path, true, false)?;
    let existing_pointer_raw = has_existing_pointer
        .then(|| std::fs::read_to_string(&pointer_path))
        .transpose()
        .with_context(|| format!("read {}", pointer_path.display()))?;
    let mut pointer = resolve_pointer_for_binary(
        client,
        Some(client.base()),
        path,
        &bytes,
        relative,
        &slug,
        base,
        dropped_into,
    )
    .await?;
    if let Some(carry) = carry {
        carry_recorded_keys(&mut pointer, carry);
    }
    if pointer.authority_id.is_none() && !crate::document_layout::is_sibling(&slug_path) {
        let folder = dropped_into
            .or_else(|| carry.and_then(|carry| carry.folder))
            .unwrap_or_else(|| pointer.matter_folder());
        pointer.folder = crate::document_layout::recorded_folder(&pointer.kind, folder);
    }
    commit_pointer_for_source(
        root,
        path,
        &pointer_path,
        &pointer,
        &bytes,
        existing_pointer_raw.as_deref(),
        has_existing_pointer,
    )
}

/// How `project sync` is run: `--dry-run`, and who may say yes.
pub(crate) struct SyncMode {
    pub(crate) dry_run: bool,
    pub(crate) approval: Approval,
    /// `--yes-delete`: the only flag that answers a deletion. `--yes` does not.
    pub(crate) yes_delete: bool,
    pub(crate) scope: DocumentScope,
}

async fn project_sync(
    root: &Path,
    dry_run: bool,
    yes: bool,
    yes_delete: bool,
    only: Option<&Path>,
) -> Result<()> {
    let root = root.canonicalize().context("resolve checkout")?;
    let scope = DocumentScope::parse(&root, only)?;
    recover_interrupted_pull_scoped(&root, &scope)?;
    let (project, host) = read_manifest(&root)?;
    let client = DocumentClient::connect(host.as_deref(), &project).await?;
    let approval = if yes {
        Approval::Yes
    } else if std::io::stdin().is_terminal() {
        Approval::Ask(Box::new(TerminalPrompter::new(
            std::io::BufReader::new(std::io::stdin()),
            std::io::stderr(),
        )))
    } else {
        Approval::Refuse
    };
    let scoped = only.is_some();
    reconcile_project(
        &root,
        &client,
        SyncMode {
            dry_run,
            approval,
            yes_delete,
            scope,
        },
    )
    .await?;
    // A run scoped to one document path leaves the Project's pages and
    // caption alone.
    if scoped {
        return Ok(());
    }
    sync_pages(&root, &client, dry_run).await?;
    sync_matter(&root, &client, dry_run).await
}

/// Store the manifest's `pages:` with the Project, so the matter page renders
/// from Navigator rather than reading git at request time. The stored set
/// becomes exactly the declared block: a page removed from it is removed
/// there, and `pages: []` clears them all. A manifest with no `pages:` key
/// declares nothing about pages, so the run leaves the stored set alone.
async fn sync_pages(root: &Path, client: &DocumentClient, dry_run: bool) -> Result<()> {
    let path = root.join(crate::projects::manifest::FILE);
    let contents =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if !declares(&contents, rules::pages::KEY) {
        return Ok(());
    }
    let manifest = crate::projects::manifest::parse(&contents).map_err(|error| anyhow!(error))?;
    let count = manifest.pages.len();
    if dry_run {
        println!("would store {count} matter page(s)");
        return Ok(());
    }
    let stored = client.put_pages(&manifest.pages).await?;
    println!("{stored} matter page(s) stored");
    Ok(())
}

/// Store the manifest's `matter:` with the Project as its Matter Caption, so
/// the matter page's caption and party roster render from Navigator rather
/// than reading git at request time. The stored caption becomes exactly the
/// declared block, and an explicit empty block (`matter: {}` or `matter:`
/// with no value) removes it. A manifest with no `matter:` key declares
/// nothing about the caption, so the run leaves the stored one alone.
async fn sync_matter(root: &Path, client: &DocumentClient, dry_run: bool) -> Result<()> {
    let path = root.join(crate::projects::manifest::FILE);
    let contents =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let matter = match declared_matter(&contents)? {
        MatterBlock::Absent => return Ok(()),
        MatterBlock::Empty => None,
        MatterBlock::Declared(matter) => Some(*matter),
    };
    if dry_run {
        println!(
            "{}",
            if matter.is_some() {
                "would store the matter caption"
            } else {
                "would clear the matter caption"
            }
        );
        return Ok(());
    }
    if client.put_matter(matter.as_ref()).await? {
        println!("matter caption stored");
    } else {
        println!("matter caption cleared");
    }
    Ok(())
}

/// Whether a manifest carries a top-level `key` at all.
fn declares(contents: &str, key: &str) -> bool {
    serde_yaml::from_str::<serde_yaml::Value>(contents)
        .ok()
        .and_then(|document| {
            document
                .as_mapping()
                .map(|mapping| mapping.contains_key(key))
        })
        .unwrap_or(false)
}

/// What a manifest says about its `matter:` block.
#[derive(Debug, PartialEq, Eq)]
enum MatterBlock {
    /// No `matter:` key: the stored caption is left alone.
    Absent,
    /// `matter: {}` or `matter:` with no value: the stored caption is cleared.
    Empty,
    Declared(Box<word::matter::Matter>),
}

/// The manifest's `matter:` block.
fn declared_matter(contents: &str) -> Result<MatterBlock> {
    let key = crate::projects::matter::KEY;
    if !declares(contents, key) {
        return Ok(MatterBlock::Absent);
    }
    let document: serde_yaml::Value = serde_yaml::from_str(contents)
        .with_context(|| format!("parse {}", crate::projects::manifest::FILE))?;
    let block = document.get(key).cloned().unwrap_or_default();
    if block.is_null() {
        return Ok(MatterBlock::Empty);
    }
    let matter: word::matter::Matter = serde_yaml::from_value(block).with_context(|| {
        format!(
            "`{key}:` in {} is malformed",
            crate::projects::manifest::FILE
        )
    })?;
    Ok(if matter.is_empty() {
        MatterBlock::Empty
    } else {
        MatterBlock::Declared(Box::new(matter))
    })
}

/// Create `documents/` and make sure Git ignores it. A checkout with no root
/// `.gitignore` gets the canonical one; one that does not ignore `documents/`
/// is refused rather than rewritten, and `project gate` names the fix.
pub(crate) fn ensure_documents_ignored(root: &Path) -> Result<()> {
    let documents = root.join(DOCUMENT_DIRECTORY);
    std::fs::create_dir_all(&documents)
        .with_context(|| format!("create {}", documents.display()))?;
    ensure_documents_tree_is_safe(&documents)?;
    let ignore = root.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, crate::projects::repository::ROOT_GITIGNORE)
            .with_context(|| format!("write {}", ignore.display()))?;
    }
    raw_document_is_git_ignored(root, &documents.join(".navigator-ignore-probe")).map_err(|_| {
        anyhow!(
            "{} does not ignore {DOCUMENT_DIRECTORY}/; run `navigator project gate` to restore the canonical root .gitignore",
            ignore.display()
        )
    })
}

/// A pointer the pre-`blobs/` layout committed beside its bytes, and the
/// [Blob](../../docs/glossary/blob.md) path it moves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyPointer {
    pub(crate) from: PathBuf,
    pub(crate) to: PathBuf,
}

/// The legacy layout a checkout still carries: every
/// `documents/<slug>.{yaml,yml}` that parses as a pointer, and the retired
/// `documents/.gitignore` guard. A YAML document that is not a pointer is a
/// client's own file and stays where it is.
pub(crate) fn legacy_layout(root: &Path) -> Result<(Vec<LegacyPointer>, Option<PathBuf>)> {
    let documents = root.join(DOCUMENT_DIRECTORY);
    let guard = documents.join(".gitignore");
    let guard = guard.is_file().then_some(guard);
    if !documents.is_dir() {
        return Ok((Vec::new(), guard));
    }
    ensure_documents_tree_is_safe(&documents)?;
    let mut pointers = Vec::new();
    for entry in walkdir::WalkDir::new(&documents).follow_links(false) {
        let entry = entry.with_context(|| format!("walk {}", documents.display()))?;
        if !entry.file_type().is_file() || !is_pointer_path(entry.path()) {
            continue;
        }
        if read_pointer(entry.path()).is_err() {
            continue;
        }
        let from = entry.into_path();
        let relative = from
            .strip_prefix(&documents)
            .map_err(|_| anyhow!("{} is outside documents/", from.display()))?;
        let slug = relative
            .to_str()
            .and_then(strip_pointer_extension)
            .ok_or_else(|| anyhow!("{} is not a pointer path", relative.display()))?;
        let to = pointer_path_for_slug(root, Path::new(slug));
        pointers.push(LegacyPointer { from, to });
    }
    pointers.sort_by(|a, b| a.from.cmp(&b.from));
    Ok((pointers, guard))
}

/// Move every legacy pointer from `documents/` to `blobs/`, normalizing the
/// retired `.yml` spelling to `.yaml`, and remove the retired
/// `documents/.gitignore`. Returns how many pointers moved. A Blob already at
/// the target with different bytes is a conflict and stops the move before
/// anything is touched; the same bytes mean the legacy copy is a leftover.
pub(crate) fn migrate_legacy_layout(root: &Path) -> Result<usize> {
    let (pointers, guard) = legacy_layout(root)?;
    for pointer in &pointers {
        if let Ok(existing) = std::fs::read(&pointer.to) {
            let legacy = std::fs::read(&pointer.from)
                .with_context(|| format!("read {}", pointer.from.display()))?;
            if existing != legacy {
                return Err(anyhow!(
                    "{} and {} both name this document with different contents; keep one and delete the other",
                    display_relative(root, &pointer.from),
                    display_relative(root, &pointer.to)
                ));
            }
        }
    }
    let blobs = root.join(POINTER_DIRECTORY);
    for pointer in &pointers {
        ensure_document_path_is_safe(root, &pointer.from, false, true)?;
        if let Some(parent) = pointer.to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        ensure_documents_tree_is_safe(&blobs)?;
        ensure_document_path_is_safe(root, &pointer.to, true, false)?;
        if pointer.to.exists() {
            std::fs::remove_file(&pointer.from)
                .with_context(|| format!("remove {}", pointer.from.display()))?;
        } else {
            std::fs::rename(&pointer.from, &pointer.to).with_context(|| {
                format!(
                    "move {} to {}",
                    pointer.from.display(),
                    pointer.to.display()
                )
            })?;
        }
    }
    // The retired guard is the only thing ignoring `documents/` until the
    // root `.gitignore` does. The gate never rewrites that file (LAW-181), so
    // the guard stays until `navigator project repository sync-skills` adds
    // the line.
    if let Some(guard) = guard {
        // A checkout with no root `.gitignore` gets the canonical one, as
        // `project sync` gives it; one it already has is never rewritten.
        let ignore = root.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, crate::projects::repository::ROOT_GITIGNORE)
                .with_context(|| format!("write {}", ignore.display()))?;
        }
        if root_ignores_documents(root) {
            std::fs::remove_file(&guard).with_context(|| format!("remove {}", guard.display()))?;
        } else {
            println!(
                "kept {}: the root .gitignore does not ignore /{DOCUMENT_DIRECTORY}/ yet; run `navigator project repository sync-skills`",
                display_relative(root, &guard)
            );
        }
    }
    Ok(pointers.len())
}

/// Whether the root `.gitignore` names `documents/` itself, the line that
/// keeps every byte below it out of Git once the retired guard is gone.
pub(crate) fn root_ignores_documents(root: &Path) -> bool {
    std::fs::read_to_string(root.join(".gitignore")).is_ok_and(|contents| {
        contents.lines().map(str::trim).any(|line| {
            matches!(
                line,
                "/documents/" | "documents/" | "/documents" | "documents"
            )
        })
    })
}

/// Refuse a checkout still on the legacy layout, for a command that reads
/// the committed pointers but does not own moving them.
pub(crate) fn refuse_legacy_layout(root: &Path) -> Result<()> {
    let (legacy, guard) = legacy_layout(root)?;
    if legacy.is_empty() && guard.is_none() {
        return Ok(());
    }
    Err(legacy_layout_refusal(legacy.len(), guard.is_some()))
}

/// What a legacy layout still carries, in one phrase: `3 pointer(s) and the
/// retired documents/.gitignore`.
pub(crate) fn describe_legacy_layout(pointers: usize, guard: bool) -> String {
    match (pointers, guard) {
        (0, _) => format!("the retired {DOCUMENT_DIRECTORY}/.gitignore"),
        (count, false) => format!("{count} pointer(s)"),
        (count, true) => {
            format!("{count} pointer(s) and the retired {DOCUMENT_DIRECTORY}/.gitignore")
        }
    }
}

/// Report a legacy layout in one line, for a run that may not move it.
fn legacy_layout_refusal(pointers: usize, guard: bool) -> anyhow::Error {
    anyhow!(
        "{} still sit under {DOCUMENT_DIRECTORY}/; run `navigator project gate` or \
         `navigator project sync` without --dry-run to move to the {POINTER_DIRECTORY}/ layout",
        describe_legacy_layout(pointers, guard)
    )
}

/// Print what a dry run would do, never touching the network again or the
/// checkout.
fn report_dry_run_plans(
    plans: &[ProjectSyncPlan],
    authorities: &[AuthorityCopy],
    local: &LocalOnly,
) {
    let (mut added, mut updated, mut skipped) = (0usize, 0usize, 0usize);
    for plan in plans {
        println!("{}", dry_run_line(plan));
        if !plan.dropped_keys.is_empty() {
            println!("  {}", dropped_keys_note(plan));
        }
        match plan.decision {
            Decision::Nothing | Decision::UploadNew => skipped += 1,
            Decision::Download if !plan.pointer_target.exists() => added += 1,
            _ => updated += 1,
        }
    }
    for copy in authorities.iter().filter(|copy| copy.missing) {
        println!("would fetch {}", copy.label);
    }
    if !local.new_files.is_empty() {
        println!(
            "would ask once to upload {} new document(s):",
            local.new_files.len()
        );
        for file in &local.new_files {
            println!("  {}", file.label);
        }
    }
    for orphan in &local.orphan_pointers {
        println!("cannot reconcile {orphan}: the pointer names a document the live Project does not list");
    }
    for notice in &local.notices {
        println!("skip {notice}");
    }
    println!("{added} added, {updated} updated, {skipped} skipped");
}

/// What `project sync` did, for the closing report.
#[derive(Default)]
struct Tally {
    added: usize,
    updated: usize,
    skipped: usize,
    uploaded: usize,
    deleted: usize,
    declined: usize,
    refused: Vec<String>,
    failures: Vec<String>,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn reconcile_project(
    root: &Path,
    client: &DocumentClient,
    mode: SyncMode,
) -> Result<()> {
    // A refused inventory is never an empty one: nothing is uploaded over what
    // could not be read.
    let inventory = client
        .list_documents()
        .await
        .context("could not read the live inventory; nothing was changed")?;
    // A checkout still on the legacy layout has its pointers moved to
    // `blobs/` before anything is planned, so no pointer under `documents/`
    // is ever mistaken for new bytes to upload. A dry run touches nothing.
    let (legacy, guard) = legacy_layout(root)?;
    if !legacy.is_empty() || guard.is_some() {
        if mode.dry_run {
            return Err(legacy_layout_refusal(legacy.len(), guard.is_some()));
        }
        let moved = migrate_legacy_layout(root)?;
        println!(
            "moved {moved} pointer(s) from {DOCUMENT_DIRECTORY}/ to {POINTER_DIRECTORY}/; commit the move"
        );
    }
    let removed = RemovedPointers::scan(root);
    // LAW-189: bytes still at `documents/<slug>` move into the matter folder
    // their pointer, or for a live document with no pointer yet its live
    // kind, names. A dry run reports what would move.
    let relocation = crate::document_layout::relocate_bytes(root, mode.dry_run)?;
    relocation.report(mode.dry_run);
    if !mode.dry_run {
        let live: Vec<(PathBuf, String)> = inventory
            .iter()
            .filter_map(|document| {
                Some((
                    inventory_relative_path(document).ok()?,
                    document.kind.clone().unwrap_or_default(),
                ))
            })
            .collect();
        for movement in crate::document_layout::relocate_unpointed(root, &live, |slug| {
            removed.last_removal(slug).is_some()
        })? {
            println!("moved {} -> {}", movement.from, movement.to);
        }
    }
    // The live Service records, grouped by served revision, so each pointer
    // carries its `service:` list. A refused read keeps every hand-recorded
    // list rather than dropping it.
    let live_service = match client.list_service().await {
        Ok(rows) => {
            let mut grouped = LiveService::new();
            for row in rows {
                grouped
                    .entry(row.document_id)
                    .or_default()
                    .push(row.record());
            }
            grouped
        }
        Err(error) => {
            eprintln!(
                "could not read the live service records; committed lists are kept: {error:#}"
            );
            LiveService::new()
        }
    };
    let plans = project_sync_plans_scoped(root, inventory, &removed, &mode.scope, &live_service)?;
    let authorities = authority_copies(root, &mode.scope)?;
    let live: std::collections::BTreeSet<String> = plans
        .iter()
        .filter_map(|plan| plan.label.strip_prefix("documents/").map(str::to_string))
        .collect();
    let local = local_only_documents(root, &live, &removed, &mode.scope)?;
    if mode.dry_run {
        report_dry_run_plans(&plans, &authorities, &local);
        return Ok(());
    }

    ensure_documents_ignored(root)?;
    crate::document_layout::ensure_tree(root)?;
    for plan in &plans {
        raw_document_is_git_ignored(root, &plan.document_target)?;
    }
    for copy in &authorities {
        raw_document_is_git_ignored(root, &copy.target)?;
    }

    let mut asker = Asker::new(mode.approval);
    let mut tally = Tally::default();
    let mut steps = Vec::with_capacity(plans.len());
    for plan in &plans {
        steps.push((plan, resolve_plan(plan, &mut asker, mode.yes_delete)?));
    }
    let upload_new = if local.new_files.is_empty() {
        Resolution::Decline
    } else {
        let listing = local
            .new_files
            .iter()
            .map(|file| format!("  {}", file.label))
            .collect::<Vec<_>>()
            .join("\n");
        asker.confirm(&format!(
            "{} new document(s) have no live copy:\n{listing}\nUpload them as new documents?",
            local.new_files.len()
        ))?
    };

    for (plan, step) in &steps {
        match (step, &plan.decision) {
            (Step::Nothing, _) => tally.skipped += 1,
            (Step::Fetch, _) if !plan.pointer_target.exists() => tally.added += 1,
            (Step::Pointer | Step::Fetch, _) => tally.updated += 1,
            (Step::Declined, _) => tally.declined += 1,
            (Step::Refused, decision) => tally.refused.push(refusal(plan, *decision)),
            (Step::Unreconciled, Decision::CouldNotReconcile(reason)) => {
                println!("cannot reconcile {}: {reason}", plan.label);
            }
            (Step::Upload | Step::Delete | Step::Unreconciled, _) => {}
        }
    }
    if upload_new == Resolution::Refused {
        tally.refused.push(format!(
            "{} new document(s): upload needs approval; run in a terminal or rerun with --yes",
            local.new_files.len()
        ));
    } else if upload_new == Resolution::Decline && !local.new_files.is_empty() {
        tally.declined += local.new_files.len();
    }
    for orphan in &local.orphan_pointers {
        println!("cannot reconcile {orphan}: the pointer names a document the live Project does not list");
    }
    for notice in &local.notices {
        println!("skip {notice}");
    }
    for (plan, _) in steps
        .iter()
        .filter(|(plan, step)| matches!(step, Step::Fetch) && !plan.dropped_keys.is_empty())
    {
        println!("{}", dropped_keys_note(plan));
    }

    let fetches: Vec<(&ProjectSyncPlan, bool)> = steps
        .iter()
        .filter_map(|(plan, step)| match step {
            Step::Fetch => Some((*plan, true)),
            Step::Pointer => Some((*plan, false)),
            _ => None,
        })
        .collect();
    let staging = tempfile::tempdir().context("create project sync staging area")?;
    let (staged, unreadable) = stage_project_sync(&fetches, &authorities, client, &staging).await?;
    tally.failures.extend(unreadable);
    if !staged.is_empty() {
        let mut transaction = begin_pull_transaction(root, staging, staged)?;
        match publish_pull_transaction(root, &mut transaction) {
            Ok(_) => {}
            Err(PublicationFailure::BeforeCommit(error)) => {
                rollback_pull_transaction(root, &transaction.path, &transaction.state)?;
                return Err(error);
            }
            Err(PublicationFailure::AfterCommit(error)) => return Err(error),
        }
    }

    for (plan, step) in &steps {
        if *step != Step::Upload {
            continue;
        }
        let base = plan.committed.as_ref().or(Some(&plan.pointer));
        match upload_local_revision(
            client,
            root,
            &plan.document_target,
            base,
            plan.committed.as_ref(),
        )
        .await
        {
            Ok(()) => tally.uploaded += 1,
            Err(error) => tally
                .failures
                .push(format!("{}: could not write: {error:#}", plan.label)),
        }
    }
    if upload_new == Resolution::Apply {
        for file in &local.new_files {
            match upload_local_revision(client, root, &file.path, None, None).await {
                Ok(()) => tally.uploaded += 1,
                Err(error) => tally
                    .failures
                    .push(format!("{}: could not write: {error:#}", file.label)),
            }
        }
    }

    for (plan, step) in &steps {
        if *step != Step::Delete {
            continue;
        }
        let commit = plan.removed_in.as_deref().unwrap_or_default();
        match client
            .soft_delete(plan.pointer.current_version.asset_id, commit)
            .await
        {
            Ok(_) => {
                tally.deleted += 1;
                // Remember the deletion was carried out, so restoring the
                // document brings its pointer back instead of asking again.
                if let Some(slug) = plan.slug.as_deref() {
                    if let Err(error) = record_actioned(root, slug, commit) {
                        eprintln!("{}: could not record the deletion: {error}", plan.label);
                    }
                }
            }
            Err(error) => tally
                .failures
                .push(format!("{}: could not write: {error:#}", plan.label)),
        }
    }

    let to_fetch = authorities.iter().filter(|copy| copy.missing).count();
    println!(
        "{} added, {} updated, {} skipped",
        tally.added, tally.updated, tally.skipped
    );
    if to_fetch > 0 {
        println!("{to_fetch} authority cop(ies) fetched");
    }
    if tally.uploaded > 0 {
        println!("{} uploaded", tally.uploaded);
    }
    if tally.deleted > 0 {
        println!(
            "{} soft-deleted; restore with `navigator site document restore` within {RETENTION_DAYS} days",
            tally.deleted
        );
    }
    if tally.declined > 0 {
        println!("{} declined; the next run asks again", tally.declined);
    }
    for failure in tally.failures.iter().chain(&tally.refused) {
        eprintln!("{failure}");
    }
    cleanup_filed_sidecars(root, &mode.scope)?;
    // A new file dropped outside every matter folder now has a pointer; its
    // bytes follow it into the folder that pointer names.
    crate::document_layout::relocate_bytes(root, false)?.report(false);
    if tally.failures.is_empty() && tally.refused.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        "{} document(s) could not be reconciled; nothing was overwritten without approval",
        tally.failures.len() + tally.refused.len()
    ))
}

fn refusal(plan: &ProjectSyncPlan, decision: Decision) -> String {
    let action = match decision {
        Decision::SoftDelete => {
            "deleting needs a yes at a terminal, or --yes-delete (--yes does not cover a deletion)"
        }
        Decision::Conflict => {
            "a conflict needs you to choose a side; run in a terminal (--yes cannot choose one)"
        }
        Decision::Upload => {
            "uploading local edits needs approval; run in a terminal or rerun with --yes"
        }
        _ => "overwriting the local copy needs approval; run in a terminal or rerun with --yes",
    };
    format!("{}: {action}", plan.label)
}

/// The local path a Blob's own bytes belong at — `blobs/<slug>.yaml` maps to
/// `documents/<slug>`. Refuses to resolve outside `<root>/documents/`, the
/// same lexical guard `document get` uses in the other direction
/// (`document_read::refuse_destination_in_documents`).
pub(crate) fn pull_target(root: &Path, pointer_relative: &Path) -> Result<PathBuf> {
    let slug = slug_for_pointer(pointer_relative).ok_or_else(|| {
        anyhow!(
            "{} does not name a pointer under {POINTER_DIRECTORY}/ (expected one of: {})",
            pointer_relative.display(),
            POINTER_READ_EXTENSIONS
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    })?;
    let documents_dir = crate::document_read::lexical(root, Path::new(DOCUMENT_DIRECTORY));
    let resolved = crate::document_read::lexical(&documents_dir, &slug);
    if !resolved.starts_with(&documents_dir) {
        return Err(anyhow!(
            "refusing to write {} outside {}",
            resolved.display(),
            documents_dir.display()
        ));
    }
    Ok(resolved)
}

/// Whether the file at `path` already carries `expected_sha256` — a missing
/// file never matches, so a fresh clone always pulls.
fn matches_digest(path: &Path, expected_sha256: &str) -> Result<bool> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(store::documents::sha256_hex(&bytes) == expected_sha256),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum PullTransactionPhase {
    Staging,
    Prepared,
    Publishing,
    Committed,
}

#[derive(Debug, Deserialize, Serialize)]
struct PullTransactionState {
    phase: PullTransactionPhase,
    targets: Vec<PullTransactionTarget>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PullTransactionTarget {
    target: PathBuf,
    backup: PathBuf,
    existed: bool,
}

struct StagedPull {
    target: PathBuf,
    staged: PathBuf,
}

struct PullTransaction {
    path: PathBuf,
    state: PullTransactionState,
    staged: Vec<StagedPull>,
}

enum PublicationFailure {
    BeforeCommit(anyhow::Error),
    AfterCommit(anyhow::Error),
}

fn pull_transaction_path(root: &Path) -> PathBuf {
    let root_key = store::documents::sha256_hex(root.to_string_lossy().as_bytes());
    std::env::temp_dir().join("navigator-pull").join(root_key)
}

fn pull_transaction_state_path(transaction: &Path) -> PathBuf {
    transaction.join("state.json")
}

fn write_pull_transaction_state(transaction: &Path, state: &PullTransactionState) -> Result<()> {
    let path = pull_transaction_state_path(transaction);
    let temporary = transaction.join(format!("state.tmp-{}", uuid::Uuid::now_v7()));
    let result = (|| {
        let bytes = serde_json::to_vec(state).context("serialize pull transaction state")?;
        let mut file = std::fs::File::create(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("flush {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("publish {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// A pull publishes a document's bytes under `documents/` and its Blob under
/// `blobs/` together; nothing else in the checkout is ever a pull target.
fn ensure_pull_target_parent_is_safe(root: &Path, target: &Path) -> Result<()> {
    let documents = crate::document_read::lexical(root, Path::new(DOCUMENT_DIRECTORY));
    let blobs = crate::document_read::lexical(root, Path::new(POINTER_DIRECTORY));
    if !target.starts_with(&documents) && !target.starts_with(&blobs) {
        return Err(anyhow!(
            "refusing to write {} outside {} and {}",
            target.display(),
            documents.display(),
            blobs.display()
        ));
    }
    let relative = target
        .strip_prefix(root)
        .map_err(|_| anyhow!("{} is outside {}", target.display(), root.display()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if current == target {
            break;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata_is_symlink(&metadata) => {
                return Err(anyhow!(
                    "refusing to write through symlink {}",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(anyhow!("{} is not a directory", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn ensure_pull_target_is_safe(root: &Path, target: &Path) -> Result<()> {
    ensure_pull_target_parent_is_safe(root, target)?;
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata_is_symlink(&metadata) => Err(anyhow!(
            "refusing to write through symlink {}",
            target.display()
        )),
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(anyhow!("{} is not a regular file", target.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", target.display())),
    }
}

fn remove_pull_target(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            std::fs::remove_dir(path).with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(_) => {
            std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    Ok(())
}

fn rollback_pull_transaction(
    root: &Path,
    transaction: &Path,
    state: &PullTransactionState,
) -> Result<()> {
    for entry in state.targets.iter().rev() {
        let target = crate::document_read::lexical(root, &entry.target);
        ensure_pull_target_parent_is_safe(root, &target)?;
        remove_pull_target(&target)?;
        if entry.existed {
            let backup = transaction.join(&entry.backup);
            let Some(parent) = target.parent() else {
                return Err(anyhow!("{} has no parent directory", target.display()));
            };
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
            std::fs::copy(&backup, &target)
                .with_context(|| format!("restore {}", target.display()))?;
        }
    }
    std::fs::remove_dir_all(transaction)
        .with_context(|| format!("remove pull transaction {}", transaction.display()))?;
    Ok(())
}

fn recover_interrupted_pull(root: &Path) -> Result<()> {
    let transaction = pull_transaction_path(root);
    if !transaction.exists() {
        return Ok(());
    }
    if !transaction.is_dir() {
        return Err(anyhow!(
            "pull recovery path {} is not a directory",
            transaction.display()
        ));
    }
    let state_path = pull_transaction_state_path(&transaction);
    let state = match std::fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice::<PullTransactionState>(&bytes)
            .with_context(|| format!("read {}", state_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::remove_dir_all(&transaction)
                .with_context(|| format!("remove {}", transaction.display()))?;
            return Ok(());
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", state_path.display())),
    };
    match state.phase {
        PullTransactionPhase::Staging | PullTransactionPhase::Committed => {
            std::fs::remove_dir_all(&transaction)
                .with_context(|| format!("remove {}", transaction.display()))?;
        }
        PullTransactionPhase::Prepared | PullTransactionPhase::Publishing => {
            rollback_pull_transaction(root, &transaction, &state)?;
        }
    }
    Ok(())
}

fn recover_interrupted_pull_scoped(root: &Path, scope: &DocumentScope) -> Result<()> {
    if scope.0.is_some() {
        let state_path = pull_transaction_state_path(&pull_transaction_path(root));
        if state_path.exists() {
            let bytes = std::fs::read(&state_path)?;
            let state: PullTransactionState = serde_json::from_slice(&bytes)?;
            for target in &state.targets {
                let slug = target
                    .target
                    .strip_prefix(DOCUMENT_DIRECTORY)
                    .ok()
                    .map(Path::to_path_buf)
                    .or_else(|| slug_for_pointer(&target.target))
                    .ok_or_else(|| {
                        anyhow!(
                            "interrupted publication has a target outside documents/ and blobs/"
                        )
                    })?;
                if !scope.includes(&slug) {
                    return Err(anyhow!("interrupted publication includes an out-of-scope target; run unscoped project sync to recover it"));
                }
            }
        }
    }
    recover_interrupted_pull(root)
}

fn cleanup_filed_sidecars(root: &Path, scope: &DocumentScope) -> Result<()> {
    for relative in crate::document_read::discover_pointers(root)? {
        let Some(slug) = slug_for_pointer(&relative) else {
            continue;
        };
        if !scope.includes(&slug) || authority_capture_folder(&slug).is_none() {
            continue;
        }
        let pointer_path = root.join(&relative);
        let Some(pointer) = read_pointer(&pointer_path)? else {
            continue;
        };
        if pointer.authority_id.is_none() {
            continue;
        }
        let source = pull_target(root, &relative)?;
        if source.exists() && !matches_digest(&source, &pointer.current_version.sha256)? {
            continue;
        }
        let sidecar = authority_sidecar_path(&source);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => {
                ensure_document_path_is_safe(root, &sidecar, false, true)?;
                std::fs::remove_file(&sidecar)
                    .with_context(|| format!("remove filed sidecar {}", sidecar.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", sidecar.display()))
            }
        }
    }
    Ok(())
}

fn begin_pull_transaction(
    root: &Path,
    staging: tempfile::TempDir,
    staged: Vec<StagedPull>,
) -> Result<PullTransaction> {
    let transaction = pull_transaction_path(root);
    let parent = transaction
        .parent()
        .ok_or_else(|| anyhow!("pull transaction has no parent directory"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create pull transaction parent {}", parent.display()))?;
    let staging_path = staging.keep();
    if let Err(error) = std::fs::rename(&staging_path, &transaction) {
        let _ = std::fs::remove_dir_all(&staging_path);
        return Err(error)
            .with_context(|| format!("move pull staging area {}", transaction.display()));
    }

    let state = PullTransactionState {
        phase: PullTransactionPhase::Staging,
        targets: Vec::new(),
    };
    if let Err(error) = write_pull_transaction_state(&transaction, &state) {
        let _ = std::fs::remove_dir_all(&transaction);
        return Err(error);
    }

    let backups = transaction.join("backups");
    if let Err(error) = std::fs::create_dir_all(&backups) {
        let _ = std::fs::remove_dir_all(&transaction);
        return Err(error).with_context(|| format!("create {}", backups.display()));
    }

    let mut prepared = state;
    for (index, item) in staged.iter().enumerate() {
        ensure_pull_target_is_safe(root, &item.target)?;
        let target = item
            .target
            .strip_prefix(root)
            .map_err(|_| anyhow!("{} is outside {}", item.target.display(), root.display()))?
            .to_path_buf();
        let existed = match std::fs::symlink_metadata(&item.target) {
            Ok(metadata) if metadata.file_type().is_file() => true,
            Ok(_) => {
                return Err(anyhow!("{} is not a regular file", item.target.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", item.target.display()));
            }
        };
        let backup = PathBuf::from(format!("backups/{index}"));
        if existed {
            std::fs::copy(&item.target, transaction.join(&backup))
                .with_context(|| format!("backup {}", item.target.display()))?;
        }
        prepared.targets.push(PullTransactionTarget {
            target,
            backup,
            existed,
        });
    }
    prepared.phase = PullTransactionPhase::Prepared;
    write_pull_transaction_state(&transaction, &prepared)?;

    let staged = staged
        .into_iter()
        .map(|item| {
            let file_name = item
                .staged
                .file_name()
                .ok_or_else(|| anyhow!("staged pull has no file name"))?;
            Ok(StagedPull {
                staged: transaction.join("downloads").join(file_name),
                target: item.target,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PullTransaction {
        path: transaction,
        state: prepared,
        staged,
    })
}

fn publish_pull_transaction(
    root: &Path,
    transaction: &mut PullTransaction,
) -> std::result::Result<usize, PublicationFailure> {
    transaction.state.phase = PullTransactionPhase::Publishing;
    write_pull_transaction_state(&transaction.path, &transaction.state)
        .map_err(PublicationFailure::BeforeCommit)?;

    for item in &transaction.staged {
        ensure_pull_target_is_safe(root, &item.target).map_err(PublicationFailure::BeforeCommit)?;
        let Some(parent) = item.target.parent() else {
            return Err(PublicationFailure::BeforeCommit(anyhow!(
                "{} has no parent directory",
                item.target.display()
            )));
        };
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))
            .map_err(PublicationFailure::BeforeCommit)?;
        std::fs::copy(&item.staged, &item.target)
            .with_context(|| format!("publish {}", item.target.display()))
            .map_err(PublicationFailure::BeforeCommit)?;
    }

    transaction.state.phase = PullTransactionPhase::Committed;
    write_pull_transaction_state(&transaction.path, &transaction.state)
        .map_err(PublicationFailure::BeforeCommit)?;
    let pulled = transaction.staged.len();
    if let Err(error) = std::fs::remove_dir_all(&transaction.path) {
        return Err(PublicationFailure::AfterCommit(anyhow!(
            "remove completed pull transaction {}: {error}",
            transaction.path.display()
        )));
    }
    Ok(pulled)
}

fn metadata_is_symlink(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

pub(crate) fn ensure_documents_tree_is_safe(documents: &Path) -> Result<()> {
    match std::fs::symlink_metadata(documents) {
        Ok(metadata) if metadata_is_symlink(&metadata) => {
            return Err(anyhow!(
                "refusing to use symlinked documents root {}",
                documents.display()
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(anyhow!("{} is not a directory", documents.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", documents.display()))
        }
    }
    for entry in walkdir::WalkDir::new(documents).follow_links(false) {
        let entry = entry.with_context(|| format!("walk {}", documents.display()))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        if metadata_is_symlink(&metadata) {
            return Err(anyhow!(
                "refusing to use symlink in documents tree: {}",
                entry.path().display()
            ));
        }
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(anyhow!(
                "refusing unsupported documents tree entry {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_document_path_is_safe(
    root: &Path,
    path: &Path,
    leaf_may_be_missing: bool,
    leaf_must_be_file: bool,
) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow!("{} is outside {}", path.display(), root.display()))?;
    let components = relative.components().collect::<Vec<_>>();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let leaf = index + 1 == components.len();
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata_is_symlink(&metadata) => {
                return Err(anyhow!(
                    "refusing to use symlink path {}",
                    current.display()
                ));
            }
            Ok(metadata) if !leaf && !metadata.is_dir() => {
                return Err(anyhow!("{} is not a directory", current.display()));
            }
            Ok(metadata) if leaf && leaf_must_be_file && !metadata.is_file() => {
                return Err(anyhow!("{} is not a regular file", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && leaf => {
                if leaf_may_be_missing {
                    return Ok(());
                }
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

/// Create the folders a Blob at `pointer_path` sits in, mirroring its
/// document's folders under `documents/`, once `blobs/` itself is known to be
/// a real directory tree with no symlink to write through.
fn prepare_pointer_parent(root: &Path, pointer_path: &Path) -> Result<()> {
    let blobs = root.join(POINTER_DIRECTORY);
    ensure_documents_tree_is_safe(&blobs)?;
    if let Some(parent) = pointer_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    ensure_documents_tree_is_safe(&blobs)
}

/// The Blob for the bytes at `path` below `documents/` — whichever spelling
/// is on disk, and whether it exists yet.
fn pointer_path_for_source(root: &Path, path: &Path) -> Result<(PathBuf, bool)> {
    let below = path
        .strip_prefix(root.join(DOCUMENT_DIRECTORY))
        .map_err(|_| anyhow!("{} is outside documents/", path.display()))?;
    // Bytes in a matter folder name the slug below it (LAW-189).
    let (slug, _) = crate::document_layout::slug_of_bytes(below);
    let slug = slug.as_path();
    let stem = root.join(POINTER_DIRECTORY).join(slug);
    for extension in POINTER_READ_EXTENSIONS {
        let candidate = PathBuf::from(format!("{}.{extension}", stem.display()));
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata_is_symlink(&metadata) => {
                return Err(anyhow!(
                    "refusing to use symlink pointer path {}",
                    candidate.display()
                ));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(anyhow!("{} is not a regular file", candidate.display()));
            }
            Ok(_) => return Ok((candidate, true)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", candidate.display()))
            }
        }
    }
    Ok((pointer_path_for_slug(root, slug), false))
}

fn validate_upload_receipt(
    pointer: &store::document_pointers::DocumentPointer,
    bytes: &[u8],
) -> Result<()> {
    pointer
        .validate()
        .map_err(|error| anyhow!("invalid upload pointer: {error}"))?;
    let expected_sha256 = store::documents::sha256_hex(bytes);
    if pointer.current_version.sha256 != expected_sha256 {
        return Err(anyhow!(
            "upload pointer sha256 mismatch: expected {expected_sha256}, received {}",
            pointer.current_version.sha256
        ));
    }
    let expected_size =
        i64::try_from(bytes.len()).context("uploaded byte count does not fit in pointer")?;
    if pointer.current_version.size_bytes != expected_size {
        return Err(anyhow!(
            "upload pointer byte count mismatch: expected {expected_size}, received {}",
            pointer.current_version.size_bytes
        ));
    }
    Ok(())
}

fn ensure_source_is_unchanged(path: &Path, expected: &[u8]) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect source {}", path.display()))?;
    if metadata_is_symlink(&metadata) {
        return Err(anyhow!(
            "source changed during upload: {} is now a symlink",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(anyhow!(
            "source changed during upload: {} is not a regular file",
            path.display()
        ));
    }
    let actual = std::fs::read(path).with_context(|| format!("read source {}", path.display()))?;
    if actual != expected {
        return Err(anyhow!("source changed during upload: {}", path.display()));
    }
    Ok(())
}

fn restore_pointer_after_source_change(
    path: &Path,
    previous: Option<&str>,
    had_previous: bool,
) -> Result<()> {
    if had_previous {
        write_pointer_atomically(
            path,
            previous.ok_or_else(|| anyhow!("previous pointer bytes are missing"))?,
        )
    } else {
        std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
    }
}

/// Every staged or pulled-down file below `documents/`. Pointers live under
/// `blobs/`, so nothing here is one; [`legacy_layout`] moves any left over
/// from the old layout before this runs.
fn discover(documents: &Path) -> Result<Vec<PathBuf>> {
    ensure_documents_tree_is_safe(documents)?;
    if !documents.is_dir() {
        return Ok(Vec::new());
    }
    let mut binaries = Vec::new();
    for entry in walkdir::WalkDir::new(documents).follow_links(false) {
        let entry = entry.with_context(|| format!("walk {}", documents.display()))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        if metadata_is_symlink(&metadata) {
            return Err(anyhow!(
                "refusing to use symlink in documents tree: {}",
                entry.path().display()
            ));
        }
        // `.gitignore`, and the `.DS_Store` a Finder window leaves in every
        // matter folder it opens, are never documents.
        if !entry.file_type().is_file() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // An Authority sidecar is metadata consulted by its known derived
        // path when the capture it describes is processed — never a pointer
        // to reconcile and never a binary to upload in its own right.
        if is_authority_sidecar_path(entry.path()) {
            continue;
        }
        binaries.push(entry.into_path());
    }
    binaries.sort();
    Ok(binaries)
}

fn inferred_kind(relative: &Path) -> &'static str {
    match relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
    {
        Some("pleadings") => "filing",
        Some("exhibits") => "exhibit",
        Some("agreements") => "agreement",
        Some("invoices") => "invoice",
        // ENG-859: the same folder-infers-kind convention `invoices/`
        // already had, extended to the letter that opens (`onboarding/`)
        // and closes (`offboarding/`) a matter.
        Some("onboarding") => "onboarding",
        Some("offboarding") => "offboarding",
        // LAW-60: a case-assessment memo or a scan transcript staged under
        // `documents/` had no folder that filed it as anything but
        // `unclassified`.
        Some("memos") => "memo",
        Some("transcripts") => "transcript",
        // `documents/cases/**` and `documents/rules/**` never reach this
        // default: `project sync` routes them through `sync_authority_capture` before
        // `inferred_kind` is ever called for those folders, and the Authority
        // pointer it writes back hard-codes `kind: exhibit` rather than
        // asking this function.
        _ => "unclassified",
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("txt") => "text/plain",
        // An Authority capture under `documents/cases/` or `documents/rules/`
        // is an archived HTML page more often than any other kind this
        // function names.
        Some("html" | "htm") => "text/html",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

pub(crate) fn slash_path(path: &Path) -> Result<String> {
    let parts = path
        .components()
        .map(|part| {
            part.as_os_str()
                .to_str()
                .ok_or_else(|| anyhow!("document path is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(parts.join("/"))
}

/// Reformat an RFC3339 timestamp to whole-second precision — the one
/// serialization every pointer-writing path shares (LAW-86), so `project
/// sync` and `project gate --check` cannot disagree on `created_at` and
/// alternate a pointer between the two on every run. A timestamp that fails
/// to parse is returned unchanged; `DocumentPointer::validate` is the seam
/// that rejects a genuinely malformed one.
pub(crate) fn normalize_created_at(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw).map_or_else(
        |_| raw.to_string(),
        |parsed| parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| slash_path(relative).ok())
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn write_pointer_atomically(path: &Path, yaml: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let temp = PathBuf::from(format!("{}.tmp-{}", path.display(), uuid::Uuid::now_v7()));
    std::fs::write(&temp, yaml).with_context(|| format!("write {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("publish {}", path.display()))
}

pub(crate) fn read_pointer(
    path: &Path,
) -> Result<Option<store::document_pointers::DocumentPointer>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => store::document_pointers::DocumentPointer::from_yaml(&raw)
            .with_context(|| format!("validate {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(test)]
#[path = "document_sync_interactive_tests.rs"]
mod interactive_tests;

#[cfg(test)]
mod tests {
    use super::{
        authority_capture_folder, authority_class_belongs_in_folder, content_type, inferred_kind,
        is_authority_sidecar_path, is_invoice_capture, is_invoice_filename, is_pointer_path,
        is_signed_pdf_filename, legacy_layout, matches_digest, migrate_legacy_layout,
        normalize_created_at, pointer_path_for_slug, project_sync_plans, pull_target,
        pull_transaction_path, read_manifest, recover_interrupted_pull, slug_for_pointer,
        strip_pointer_extension, validate_folder_pointer_keys, write_pull_transaction_state,
        write_uploaded_pointer, PullTransactionPhase, PullTransactionState, PullTransactionTarget,
        AUTHORITY_SIDECAR_EXTENSION, POINTER_EXTENSION, POINTER_READ_EXTENSIONS,
    };
    use crate::document_deletion::RemovedPointers;
    use std::path::Path;
    use uuid::Uuid;

    fn onboarding_pointer(
        docusign_envelope_id: Option<Uuid>,
    ) -> store::document_pointers::DocumentPointer {
        store::document_pointers::DocumentPointer {
            kind: "onboarding".into(),
            visibility: "internal".into(),
            folder: None,
            current_version: store::document_pointers::PointerVersion {
                version: 1,
                asset_id: Uuid::now_v7(),
                created_at: "2026-09-05T12:00:00Z".into(),
                sha256: "0".repeat(64),
                size_bytes: 1,
                canonical_url: None,
                checked_on: None,
            },
            previous_version: None,
            authority_id: None,
            docusign_envelope_id,
            xero_invoice_id: None,
            signing: None,
            notarization: Vec::new(),
            service: Vec::new(),
        }
    }

    fn invoice_pointer(xero_invoice_id: Option<Uuid>) -> store::document_pointers::DocumentPointer {
        store::document_pointers::DocumentPointer {
            kind: "invoice".into(),
            visibility: "internal".into(),
            folder: None,
            current_version: store::document_pointers::PointerVersion {
                version: 1,
                asset_id: Uuid::now_v7(),
                created_at: "2026-09-05T12:00:00Z".into(),
                sha256: "0".repeat(64),
                size_bytes: 1,
                canonical_url: None,
                checked_on: None,
            },
            previous_version: None,
            authority_id: None,
            docusign_envelope_id: None,
            xero_invoice_id,
            signing: None,
            notarization: Vec::new(),
            service: Vec::new(),
        }
    }

    /// LAW-86: whatever precision a server endpoint hands back, every
    /// pointer-writing path must agree on one — otherwise `project sync` and
    /// `project gate --check` alternate a pointer's `created_at` forever.
    #[test]
    fn normalize_created_at_truncates_fractional_seconds() {
        assert_eq!(
            normalize_created_at("2026-09-23T20:53:07.132412551Z"),
            "2026-09-23T20:53:07Z"
        );
        assert_eq!(
            normalize_created_at("2026-09-23T20:53:07Z"),
            "2026-09-23T20:53:07Z"
        );
        assert_eq!(
            normalize_created_at("not-a-timestamp"),
            "not-a-timestamp",
            "an unparsable value is left alone; validate() is the seam that rejects it"
        );
    }

    #[test]
    fn uploaded_document_pointers_are_written_under_documents() {
        let root = tempfile::tempdir().unwrap();
        let pointer = onboarding_pointer(Some(Uuid::now_v7()));

        let path = write_uploaded_pointer(root.path(), "client/retainer.pdf", &pointer).unwrap();

        assert_eq!(path, root.path().join("blobs/client/retainer.pdf.yaml"));
        let written = store::document_pointers::DocumentPointer::from_yaml(
            &std::fs::read_to_string(path).unwrap(),
        )
        .unwrap();
        assert_eq!(written.kind, pointer.kind);
        assert_eq!(written.docusign_envelope_id, pointer.docusign_envelope_id);
    }

    #[test]
    fn uploaded_document_pointers_reject_unsafe_slugs() {
        let root = tempfile::tempdir().unwrap();
        let pointer = onboarding_pointer(None);

        for slug in ["", "/absolute.pdf", "../outside.pdf", "a/../outside.pdf"] {
            assert!(
                write_uploaded_pointer(root.path(), slug, &pointer).is_err(),
                "expected unsafe slug to fail: {slug:?}"
            );
        }
        assert!(!root.path().join("outside.pdf.yaml").exists());
    }

    /// `project sync` builds its pointer from `GET …/documents`, which
    /// serializes `inserted_at` at its stored, sub-second precision. The
    /// pointer it writes must still land on whole-second precision, the same
    /// convention `project gate --check` uses.
    #[test]
    fn project_sync_plans_normalizes_a_fractional_second_timestamp() {
        let document = crate::remote::LiveDocumentSummary {
            id: Uuid::now_v7(),
            byte_size: 18,
            sha256_hex: "0".repeat(64),
            filename: Some("motion.pdf".into()),
            kind: Some("filing".into()),
            visibility: "internal".into(),
            slug: Some("pleadings/motion.pdf".into()),
            inserted_at: "2026-09-23T20:53:07.132412551Z".into(),
            signing: None,
            notarization: Vec::new(),
        };
        let root = tempfile::tempdir().unwrap();
        let plans =
            project_sync_plans(root.path(), vec![document], &RemovedPointers::default()).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].pointer.current_version.created_at,
            "2026-09-23T20:53:07Z"
        );
    }

    fn live_signed_letter(id: Uuid) -> crate::remote::LiveDocumentSummary {
        live_signed_letter_with_sha(id, &"0".repeat(64))
    }

    fn live_signed_letter_with_sha(id: Uuid, sha256: &str) -> crate::remote::LiveDocumentSummary {
        crate::remote::LiveDocumentSummary {
            id,
            byte_size: 18,
            sha256_hex: sha256.to_string(),
            filename: Some("letter_signed.pdf".into()),
            kind: Some("filing".into()),
            visibility: "internal".into(),
            slug: Some("onboarding/letter_signed.pdf".into()),
            inserted_at: "2026-09-23T20:53:07Z".into(),
            signing: None,
            notarization: Vec::new(),
        }
    }

    fn write_local_pointer(
        root: &Path,
        asset_id: Uuid,
        envelope: Option<Uuid>,
        invoice: Option<Uuid>,
    ) {
        let mut pointer = project_sync_plans(
            root,
            vec![live_signed_letter(asset_id)],
            &RemovedPointers::default(),
        )
        .unwrap()
        .remove(0)
        .pointer;
        pointer.docusign_envelope_id = envelope;
        pointer.xero_invoice_id = invoice;
        let target = root.join("blobs/onboarding/letter_signed.pdf.yaml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, pointer.to_yaml().unwrap()).unwrap();
    }

    /// ENG-861: the live inventory never carries the external-id keys, so a
    /// sync must keep the ones the committed pointer already records for the
    /// same revision rather than rewrite the pointer without them.
    #[test]
    fn project_sync_plans_keeps_external_ids_for_an_unchanged_revision() {
        let asset_id = Uuid::now_v7();
        let envelope = Uuid::now_v7();
        let invoice = Uuid::now_v7();
        let root = tempfile::tempdir().unwrap();
        write_local_pointer(root.path(), asset_id, Some(envelope), Some(invoice));

        let plans = project_sync_plans(
            root.path(),
            vec![live_signed_letter(asset_id)],
            &RemovedPointers::default(),
        )
        .unwrap();

        assert_eq!(plans[0].pointer.docusign_envelope_id, Some(envelope));
        assert_eq!(plans[0].pointer.xero_invoice_id, Some(invoice));
        assert!(
            !plans[0].pointer_changed,
            "an unchanged revision must not rewrite its pointer"
        );
    }

    #[test]
    fn project_sync_plans_drops_external_ids_when_the_revision_changed() {
        let envelope = Uuid::now_v7();
        let root = tempfile::tempdir().unwrap();
        write_local_pointer(root.path(), Uuid::now_v7(), Some(envelope), None);

        let plans = project_sync_plans(
            root.path(),
            vec![live_signed_letter_with_sha(Uuid::now_v7(), &"1".repeat(64))],
            &RemovedPointers::default(),
        )
        .unwrap();

        assert_eq!(plans[0].pointer.docusign_envelope_id, None);
        assert!(plans[0].pointer_changed);
        assert_eq!(
            plans[0].dropped_keys,
            vec!["docusign_envelope_id"],
            "the dropped key is named, not lost silently"
        );
    }

    /// The same bytes filed again under a new revision id are still the
    /// signed instrument, so the pointer keeps the envelope that signed them.
    #[test]
    fn project_sync_plans_keeps_external_ids_when_a_new_revision_has_the_same_bytes() {
        let envelope = Uuid::now_v7();
        let root = tempfile::tempdir().unwrap();
        write_local_pointer(root.path(), Uuid::now_v7(), Some(envelope), None);

        let plans = project_sync_plans(
            root.path(),
            vec![live_signed_letter(Uuid::now_v7())],
            &RemovedPointers::default(),
        )
        .unwrap();

        assert_eq!(plans[0].pointer.docusign_envelope_id, Some(envelope));
        assert!(plans[0].dropped_keys.is_empty());
    }

    /// The live signing record lands on the pointer; a block recorded by hand
    /// on the committed pointer survives a sync while the live row has none.
    #[test]
    fn project_sync_plans_carries_the_signing_record() {
        let asset_id = Uuid::now_v7();
        let wet_ink = store::documents::Signing {
            method: store::documents::SigningMethod::WetInk,
            signed_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
        };
        let root = tempfile::tempdir().unwrap();
        let mut live = live_signed_letter(asset_id);
        live.signing = Some(wet_ink);
        let plans =
            project_sync_plans(root.path(), vec![live], &RemovedPointers::default()).unwrap();
        assert_eq!(plans[0].pointer.signing, Some(wet_ink));

        let mut hand_recorded = project_sync_plans(
            root.path(),
            vec![live_signed_letter(asset_id)],
            &RemovedPointers::default(),
        )
        .unwrap()
        .remove(0)
        .pointer;
        hand_recorded.signing = Some(wet_ink);
        hand_recorded.notarization = vec![store::documents::Notarization {
            method: store::documents::NotarizationMethod::RemoteOnline,
            acknowledged_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
            state: "NV".into(),
            county: "Clark".into(),
            commission_expires: chrono::NaiveDate::from_ymd_opt(2029, 1, 31).unwrap(),
        }];
        let target = root.path().join("blobs/onboarding/letter_signed.pdf.yaml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, hand_recorded.to_yaml().unwrap()).unwrap();

        let plans = project_sync_plans(
            root.path(),
            vec![live_signed_letter(asset_id)],
            &RemovedPointers::default(),
        )
        .unwrap();
        assert_eq!(plans[0].pointer.signing, Some(wet_ink));
        assert!(!plans[0].pointer_changed);
    }

    #[test]
    fn is_pointer_path_excludes_the_authority_sidecar_extension() {
        assert_eq!(AUTHORITY_SIDECAR_EXTENSION, "authority.yaml");
        assert!(is_pointer_path(Path::new("blobs/cases/roe.html.yaml")));
        assert!(!is_pointer_path(Path::new(
            "documents/cases/roe.html.authority.yaml"
        )));
        assert!(is_authority_sidecar_path(Path::new(
            "documents/cases/roe.html.authority.yaml"
        )));
        assert!(!is_authority_sidecar_path(Path::new(
            "blobs/cases/roe.html.yaml"
        )));
    }

    #[test]
    fn authority_and_invoice_captures_are_recognized_by_their_top_folder() {
        assert_eq!(
            authority_capture_folder(Path::new("cases/roe.html")),
            Some("cases")
        );
        assert_eq!(
            authority_capture_folder(Path::new("rules/nrs-86-201.html")),
            Some("rules")
        );
        assert_eq!(
            authority_capture_folder(Path::new("pleadings/motion.pdf")),
            None
        );
        assert!(is_invoice_capture(Path::new("invoices/INV-1.pdf")));
        assert!(!is_invoice_capture(Path::new("exhibits/photo.png")));
    }

    #[test]
    fn authority_class_must_belong_in_its_folder() {
        assert!(authority_class_belongs_in_folder("case_law", "cases"));
        assert!(!authority_class_belongs_in_folder("statute", "cases"));
        assert!(authority_class_belongs_in_folder("statute", "rules"));
        assert!(authority_class_belongs_in_folder("regulation", "rules"));
        assert!(authority_class_belongs_in_folder("administrative", "rules"));
        assert!(authority_class_belongs_in_folder("secondary", "rules"));
        assert!(!authority_class_belongs_in_folder("case_law", "rules"));
    }

    #[test]
    fn inferred_kind_maps_every_folder_convention() {
        assert_eq!(inferred_kind(Path::new("pleadings/motion.pdf")), "filing");
        assert_eq!(inferred_kind(Path::new("exhibits/photo.png")), "exhibit");
        assert_eq!(inferred_kind(Path::new("agreements/nda.pdf")), "agreement");
        assert_eq!(inferred_kind(Path::new("invoices/INV-1.pdf")), "invoice");
        // ENG-859: `onboarding/` and `offboarding/` are recognized folders,
        // the same way `invoices/` already is.
        assert_eq!(
            inferred_kind(Path::new("onboarding/retainer_signed.pdf")),
            "onboarding"
        );
        assert_eq!(
            inferred_kind(Path::new("offboarding/closing_signed.pdf")),
            "offboarding"
        );
        // LAW-60: a case-assessment memo or a scan transcript staged under
        // `documents/memos/` or `documents/transcripts/` now files as its
        // own kind rather than falling into `unclassified`.
        assert_eq!(inferred_kind(Path::new("memos/case-assessment.md")), "memo");
        assert_eq!(
            inferred_kind(Path::new("transcripts/deposition.md")),
            "transcript"
        );
        assert_eq!(inferred_kind(Path::new("misc/note.txt")), "unclassified");
    }

    #[test]
    fn invoice_filenames_must_match_the_synthetic_pattern() {
        assert!(is_invoice_filename("INV-1.pdf"));
        assert!(is_invoice_filename("INV-042.pdf"));
        assert!(!is_invoice_filename("invoice-1.pdf"));
        assert!(!is_invoice_filename("INV-.pdf"));
        assert!(!is_invoice_filename("INV-1a.pdf"));
        assert!(!is_invoice_filename("INV-1"));
    }

    #[test]
    fn signed_pdf_filenames_require_a_non_empty_stem() {
        assert!(is_signed_pdf_filename("retainer_signed.pdf"));
        assert!(!is_signed_pdf_filename("retainer.pdf"));
        assert!(!is_signed_pdf_filename("_signed.pdf"));
        assert!(!is_signed_pdf_filename("retainer_signed.PDF"));
    }

    #[test]
    fn a_signed_onboarding_pointer_needs_a_matching_name_and_envelope_id() {
        // Matching name, envelope id present: valid.
        assert!(validate_folder_pointer_keys(
            Path::new("onboarding/retainer_signed.pdf"),
            &onboarding_pointer(Some(Uuid::now_v7())),
        )
        .is_ok());
        // `_signed.pdf` name, no envelope id: rejected.
        assert!(validate_folder_pointer_keys(
            Path::new("onboarding/retainer_signed.pdf"),
            &onboarding_pointer(None),
        )
        .is_err());
        // Envelope id present, name does not end `_signed.pdf`: rejected.
        assert!(validate_folder_pointer_keys(
            Path::new("onboarding/retainer.pdf"),
            &onboarding_pointer(Some(Uuid::now_v7())),
        )
        .is_err());
        // An unsigned draft beside the signed copy: no envelope id required.
        assert!(validate_folder_pointer_keys(
            Path::new("onboarding/retainer.pdf"),
            &onboarding_pointer(None),
        )
        .is_ok());
        // The same rule applies to offboarding.
        assert!(validate_folder_pointer_keys(
            Path::new("offboarding/closing_signed.pdf"),
            &onboarding_pointer(None),
        )
        .is_err());
    }

    #[test]
    fn every_invoice_pointer_needs_a_xero_invoice_id() {
        assert!(validate_folder_pointer_keys(
            Path::new("invoices/INV-1.pdf"),
            &invoice_pointer(Some(Uuid::now_v7())),
        )
        .is_ok());
        assert!(validate_folder_pointer_keys(
            Path::new("invoices/INV-1.pdf"),
            &invoice_pointer(None),
        )
        .is_err());
    }

    #[test]
    fn html_authority_captures_get_a_real_content_type() {
        assert_eq!(content_type(Path::new("roe.html")), "text/html");
        assert_eq!(content_type(Path::new("roe.htm")), "text/html");
    }

    #[test]
    fn only_a_manifest_with_a_pages_key_syncs_pages() {
        assert!(!super::declares(
            "version: 26.10.6\nproject:\n  name: acme\n",
            rules::pages::KEY
        ));
        assert!(super::declares("pages: []\n", rules::pages::KEY));
        assert!(!super::declares("not: [valid", rules::pages::KEY));
    }

    #[test]
    fn an_absent_matter_key_leaves_the_caption_and_an_empty_one_clears_it() {
        use super::MatterBlock;
        assert_eq!(
            super::declared_matter("project:\n  name: acme\n").unwrap(),
            MatterBlock::Absent
        );
        assert_eq!(
            super::declared_matter("matter: {}\n").unwrap(),
            MatterBlock::Empty
        );
        assert_eq!(
            super::declared_matter("matter:\n").unwrap(),
            MatterBlock::Empty
        );
        let MatterBlock::Declared(declared) = super::declared_matter(
            "matter:\n  case_number: 00CV000000\n  parties:\n    - name: A CORP\n      \
             role: Plaintiff\n",
        )
        .unwrap() else {
            panic!("a declared block");
        };
        assert_eq!(declared.case_number, "00CV000000");
        assert_eq!(declared.parties[0].name, "A CORP");
        let error = super::declared_matter("matter:\n  docket: 1\n").unwrap_err();
        assert!(format!("{error:#}").contains("malformed"), "{error:#}");
    }

    #[test]
    fn read_manifest_yields_project_and_host_from_a_v2_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "version: \"1.0.0\"\nproject:\n  host: staging.neonlaw.com\n  name: acme\n",
        )
        .unwrap();

        let (project, host) = read_manifest(dir.path()).unwrap();

        assert_eq!(project, "acme");
        assert_eq!(host, Some("staging.neonlaw.com".to_string()));
    }

    #[test]
    fn read_manifest_still_yields_project_and_host_from_the_deprecated_flat_form() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project: acme\nhost: staging.neonlaw.com\n",
        )
        .unwrap();

        let (project, host) = read_manifest(dir.path()).unwrap();

        assert_eq!(project, "acme");
        assert_eq!(host, Some("staging.neonlaw.com".to_string()));
    }

    #[test]
    fn read_manifest_reports_one_error_against_navigator_yaml_for_a_malformed_project_map() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  name: acme\n",
        )
        .unwrap();

        let error = read_manifest(dir.path()).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("navigator.yaml"), "{message}");
        assert_eq!(message.matches("navigator.yaml").count(), 1, "{message}");
    }

    #[test]
    fn pull_target_strips_either_pointer_extension_and_stays_below_documents() {
        let root = Path::new("/repo");
        // Both spellings resolve to the same staging path. `.yml` is the
        // retired one and stays readable so a fleet-wide rename does not have
        // to be atomic with the release (LAW-25).
        for pointer in [
            "blobs/pleadings/motion.pdf.yaml",
            "blobs/pleadings/motion.pdf.yml",
        ] {
            assert_eq!(
                pull_target(root, Path::new(pointer)).unwrap(),
                Path::new("/repo/documents/pleadings/motion.pdf"),
                "{pointer} must resolve to the document beside it"
            );
        }
    }

    #[test]
    fn pull_target_refuses_a_path_that_names_no_pointer_and_says_what_it_wanted() {
        let root = Path::new("/repo");
        let error = pull_target(root, Path::new("documents/pleadings/motion.pdf")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(".yaml"), "{message}");
        assert!(message.contains(".yml"), "{message}");
    }

    #[test]
    fn the_pointer_extension_contract_writes_yaml_and_reads_both() {
        assert_eq!(
            POINTER_EXTENSION, "yaml",
            "pointers are written at the extension every other Navigator YAML file uses"
        );
        assert_eq!(
            POINTER_READ_EXTENSIONS,
            &["yaml", "yml"],
            "the retired spelling stays readable through the rename"
        );
        assert!(is_pointer_path(Path::new("blobs/a.pdf.yaml")));
        assert!(is_pointer_path(Path::new("blobs/a.pdf.yml")));
        assert!(!is_pointer_path(Path::new("documents/a.pdf")));
        assert_eq!(strip_pointer_extension("a.pdf.yaml"), Some("a.pdf"));
        assert_eq!(strip_pointer_extension("a.pdf.yml"), Some("a.pdf"));
        assert_eq!(strip_pointer_extension("a.pdf"), None);
    }

    /// The gitignore admits only the written spelling. The retired `.yml`
    /// spelling stays *readable* via [`POINTER_READ_EXTENSIONS`] for pointers
    /// committed before the rename; this only keeps a fresh one from being
    /// swept into Git by an unqualified `git add`.
    #[test]
    fn the_root_gitignore_ignores_the_whole_documents_folder() {
        let ignore = crate::projects::repository::ROOT_GITIGNORE;
        assert!(
            ignore.lines().any(|line| line == "/documents/"),
            "the root .gitignore must ignore documents/ outright, got {ignore:?}"
        );
        assert!(
            ignore
                .lines()
                .filter(|line| !line.starts_with('#'))
                .all(|line| !line.contains("blobs")),
            "blobs/ is committed and must never be ignored, got {ignore:?}"
        );
    }

    #[test]
    fn a_blob_mirrors_its_document_path_for_path() {
        let root = Path::new("/repo");
        let pointer = pointer_path_for_slug(root, Path::new("pleadings/motion.pdf"));
        assert_eq!(pointer, Path::new("/repo/blobs/pleadings/motion.pdf.yaml"));
        assert_eq!(
            slug_for_pointer(Path::new("blobs/pleadings/motion.pdf.yaml")).unwrap(),
            Path::new("pleadings/motion.pdf")
        );
        assert!(slug_for_pointer(Path::new("documents/pleadings/motion.pdf.yaml")).is_none());
    }

    fn legacy_pointer_yaml() -> String {
        invoice_pointer(None).to_yaml().unwrap()
    }

    #[test]
    fn legacy_pointers_move_to_blobs_and_leave_the_bytes_and_other_yaml_alone() {
        let root = tempfile::tempdir().unwrap();
        let documents = root.path().join("documents");
        std::fs::create_dir_all(documents.join("pleadings")).unwrap();
        std::fs::write(
            documents.join(".gitignore"),
            "*\n!*/\n!*.yaml\n!.gitignore\n",
        )
        .unwrap();
        std::fs::write(documents.join("pleadings/motion.pdf"), b"%PDF").unwrap();
        std::fs::write(
            documents.join("pleadings/motion.pdf.yaml"),
            legacy_pointer_yaml(),
        )
        .unwrap();
        std::fs::write(documents.join("old.pdf.yml"), legacy_pointer_yaml()).unwrap();
        // A client's own YAML file is a document, not a pointer.
        std::fs::write(documents.join("config.yaml"), "name: client\n").unwrap();
        std::fs::write(
            root.path().join(".gitignore"),
            crate::projects::repository::ROOT_GITIGNORE,
        )
        .unwrap();

        let (legacy, guard) = legacy_layout(root.path()).unwrap();
        assert_eq!(legacy.len(), 2);
        assert!(guard.is_some());

        assert_eq!(migrate_legacy_layout(root.path()).unwrap(), 2);
        assert!(root
            .path()
            .join("blobs/pleadings/motion.pdf.yaml")
            .is_file());
        assert!(
            root.path().join("blobs/old.pdf.yaml").is_file(),
            "the retired .yml spelling is normalized as it moves"
        );
        assert!(!documents.join("pleadings/motion.pdf.yaml").exists());
        assert!(!documents.join(".gitignore").exists());
        assert!(documents.join("pleadings/motion.pdf").is_file());
        assert!(documents.join("config.yaml").is_file());
        assert_eq!(legacy_layout(root.path()).unwrap(), (Vec::new(), None));
    }

    /// LAW-181: the gate never rewrites the root `.gitignore`, so the retired
    /// guard is the only thing ignoring `documents/` until that file names
    /// it. The move keeps the guard rather than expose the bytes.
    #[test]
    fn the_retired_guard_stays_until_the_root_gitignore_ignores_documents() {
        let root = tempfile::tempdir().unwrap();
        let documents = root.path().join("documents");
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(
            documents.join(".gitignore"),
            "*\n!*/\n!*.yaml\n!.gitignore\n",
        )
        .unwrap();
        std::fs::write(documents.join("motion.pdf.yaml"), legacy_pointer_yaml()).unwrap();
        std::fs::write(root.path().join(".gitignore"), "notes/\n").unwrap();

        assert_eq!(migrate_legacy_layout(root.path()).unwrap(), 1);
        assert!(root.path().join("blobs/motion.pdf.yaml").is_file());
        assert!(documents.join(".gitignore").is_file(), "the guard is kept");

        std::fs::write(root.path().join(".gitignore"), "notes/\n/documents/\n").unwrap();
        migrate_legacy_layout(root.path()).unwrap();
        assert!(!documents.join(".gitignore").exists());
    }

    #[test]
    fn a_legacy_pointer_that_disagrees_with_its_blob_moves_nothing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("documents")).unwrap();
        std::fs::create_dir_all(root.path().join("blobs")).unwrap();
        std::fs::create_dir_all(root.path().join("blobs")).unwrap();
        std::fs::write(
            root.path().join("documents/a.pdf.yaml"),
            legacy_pointer_yaml(),
        )
        .unwrap();
        std::fs::write(
            root.path().join("documents/b.pdf.yaml"),
            legacy_pointer_yaml(),
        )
        .unwrap();
        let other = invoice_pointer(Some(Uuid::nil())).to_yaml().unwrap();
        std::fs::write(root.path().join("blobs/b.pdf.yaml"), &other).unwrap();

        let error = migrate_legacy_layout(root.path()).unwrap_err();
        assert!(error.to_string().contains("different contents"), "{error}");
        assert!(root.path().join("documents/a.pdf.yaml").is_file());
        assert_eq!(
            std::fs::read_to_string(root.path().join("blobs/b.pdf.yaml")).unwrap(),
            other
        );
    }

    #[test]
    fn pull_target_refuses_a_pointer_that_would_escape_the_checkout() {
        let root = Path::new("/repo");
        let error = pull_target(root, Path::new("blobs/../../etc/passwd.yml")).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");
    }

    #[test]
    fn matches_digest_is_false_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!matches_digest(&dir.path().join("absent.pdf"), &"a".repeat(64)).unwrap());
    }

    #[test]
    fn matches_digest_compares_the_actual_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("present.pdf");
        std::fs::write(&path, b"hello").unwrap();
        let sha256 = store::documents::sha256_hex(b"hello");
        assert!(matches_digest(&path, &sha256).unwrap());
        assert!(!matches_digest(&path, &"0".repeat(64)).unwrap());
    }

    #[test]
    fn recovery_restores_the_before_state_after_an_interrupted_publication() {
        let root = tempfile::tempdir().unwrap();
        let transaction = pull_transaction_path(root.path());
        let backups = transaction.join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        let first_target = root.path().join("documents/pleadings/a.pdf");
        let second_target = root.path().join("documents/pleadings/b.pdf");
        std::fs::create_dir_all(first_target.parent().unwrap()).unwrap();
        std::fs::write(&first_target, b"before first").unwrap();
        std::fs::write(&second_target, b"before second").unwrap();
        std::fs::copy(&first_target, backups.join("0")).unwrap();
        std::fs::copy(&second_target, backups.join("1")).unwrap();
        let state = PullTransactionState {
            phase: PullTransactionPhase::Publishing,
            targets: vec![
                PullTransactionTarget {
                    target: Path::new("documents/pleadings/a.pdf").to_path_buf(),
                    backup: Path::new("backups/0").to_path_buf(),
                    existed: true,
                },
                PullTransactionTarget {
                    target: Path::new("documents/pleadings/b.pdf").to_path_buf(),
                    backup: Path::new("backups/1").to_path_buf(),
                    existed: true,
                },
            ],
        };
        write_pull_transaction_state(&transaction, &state).unwrap();
        std::fs::write(&first_target, b"after first").unwrap();

        recover_interrupted_pull(root.path()).unwrap();

        assert_eq!(std::fs::read(&first_target).unwrap(), b"before first");
        assert_eq!(std::fs::read(&second_target).unwrap(), b"before second");
        assert!(!transaction.exists());
    }
}
