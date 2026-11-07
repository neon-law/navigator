//! Authenticated HTTP client for the `navigator` CLI.
//!
//! Every command here is a thin wrapper over an **existing** `web` route,
//! sent with `Authorization: Bearer <token>` — no parallel JSON API. The
//! server resolves the bearer back into the caller's session and runs the
//! same handler the browser does, so `is_lawyer_tier`, the `lawyer_review`
//! gate, and `authored_by` provenance all hold unchanged.
//!
//! | command | route |
//! | --- | --- |
//! | `projects create` | `POST /app/api/projects` (plus `GET /app/api/people`, `/entities`, `/entity-types`, `/jurisdictions`) |
//! | `projects close` | `POST /app/api/projects/{id}/lifecycle`, then a repository archive upload |
//! | `project setup` | `GET /app/api/projects` plus the authenticated surface, Slack, and Notion setup doors |
//! | `project sync` | `GET /app/api/projects/{id}/documents` plus each current revision's download door |
//! | `document upload` | `POST /app/api/projects/{id}/documents`, or above the buffered cap the streaming session doors — `POST .../documents/upload-session`, `.../upload-session/chunk`, `.../upload-session/status`, `.../upload-session/complete` (LAW-172; session ids ride in bodies, never URLs) |
//! | `document slug` | `PATCH /app/api/projects/{id}/documents/{asset_id}` |
//! | `document reclassify` | `POST /app/api/projects/{id}/documents/{asset_id}/reclassify` |
//! | `document repair` | `POST /app/api/projects/{id}/documents/{asset_id}/storage` |
//! | `notation create`  | `POST /app/projects/{project_code}/notations/new` |
//! | `notation preview` | `POST /app/projects/{project_code}/notations/draft` |
//! | `navigator site import` | `POST /app/api/seed` (optional `POST /auth/ci/seed-token`) |

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::credentials::{self, default_credentials_path, HostCredential};
use crate::login::resolve_base;
use crate::palette;

/// A definite authentication refusal, before any document was filed.
#[derive(Debug, thiserror::Error)]
#[error("the session for {0} has expired — run `navigator site login --host {0}`")]
struct UploadSessionExpired(String);

/// Resolve `(base_url, bearer_token)` for `host`, erroring clearly when
/// there's no login or the stored token has expired.
pub(crate) fn resolve(host: Option<&str>) -> Result<(String, String)> {
    let creds = credentials::load(&default_credentials_path())?;
    let base = resolve_base(host, &creds)?;
    let cred: &HostCredential = creds
        .get(&base)
        .ok_or_else(|| anyhow!("not logged in to {base} — run `navigator site login --host …`"))?;
    if cred.is_expired(now_secs()) {
        return Err(UploadSessionExpired(base).into());
    }
    Ok((base, cred.token.clone()))
}

/// Return only a current login keyed to this manifest host. Validation must
/// never fall back to the sole session for some other deployment.
pub(crate) enum MatchingSession {
    Valid(String, String),
    Absent,
    Expired,
}

pub(crate) fn matching_stored_session(host: &str) -> Result<MatchingSession> {
    let creds = credentials::load(&default_credentials_path())?;
    let base = credentials::base_url(host);
    Ok(match creds.get(&base) {
        None => MatchingSession::Absent,
        Some(credential) if credential.is_expired(now_secs()) => MatchingSession::Expired,
        Some(credential) => MatchingSession::Valid(base, credential.token.clone()),
    })
}

/// How `navigator site import` authenticates to the deployment.
pub enum SeedCredential {
    /// Bearer from `navigator site login` (`~/.navigator.json`).
    Stored { host: Option<String> },
    /// Actions OIDC exchanged at `POST /auth/ci/seed-token` (GitHub) or
    /// `POST /auth/ci/forgejo/seed-token` (Forgejo).
    Ci { host: String },
}

/// How one `POST /app/api/seed` is asked to behave.
#[derive(Clone, Copy, Default)]
pub struct SeedOptions<'a> {
    pub overwrite: bool,
    pub dry_run: bool,
    /// Ask the deployment to apply the scoped-session rules a CI-minted
    /// session would carry, scoped to this Project code, so an operator's
    /// dry run answers whether CI could perform the same write. The server
    /// refuses it on anything but a plain dry run.
    pub as_ci_project: Option<&'a str>,
}

/// The Project code `--as-ci` scopes a dry run to: the checkout's own
/// `navigator.yaml`, because that is the Project CI would be minted for.
pub fn as_ci_project_code() -> Result<String> {
    crate::projects::manifest::read(Path::new("."))
        .and_then(|manifest| manifest.project)
        .ok_or_else(|| {
            anyhow!(
                "--as-ci needs a Project checkout: navigator.yaml names the project CI is scoped to"
            )
        })
}

/// `navigator site import <model> <file> [--overwrite] [--dry-run] [--as-ci]`
/// — submit one standard seed YAML document to the logged-in deployment. The
/// CLI deliberately reads no `SurrealDB` environment: authentication,
/// authorization, lookup, and the typed write boundary all belong to the
/// server.
///
/// A large document goes in chunks of [`SEED_CHUNK_RECORDS`]. The exit code
/// says what was committed: `0` all of it, `4` some of it, `2` none.
pub async fn seed(
    credential: SeedCredential,
    model: &str,
    file: &Path,
    options: SeedOptions<'_>,
) -> ExitCode {
    run(async {
        let yaml = std::fs::read_to_string(file)
            .with_context(|| format!("read seed file {}", file.display()))?;
        let session = resolve_seed_credential(&credential).await?;
        post_seed(&session, model, &yaml, options)
            .await
            .map_err(|failure| failure.into_error(0))?;
        Ok(())
    })
    .await
}

/// One whole seed document through `POST /app/api/seed` under a stored
/// session, never overwriting: what `navigator project reference add` sends.
pub(crate) async fn send_seed_document(
    session: &(String, String),
    model: &str,
    yaml: &str,
) -> Result<String> {
    send_seed(session, model, yaml, SeedOptions::default()).await
}

/// `navigator site import` with no `MODEL_NAME`/`SEED_FILE` — submit every
/// supported seed document in `dir`, in [`store::seed::SeedModel::ALL`]
/// order. A missing `dir` prints a notice and succeeds, the same convention
/// `project portal` uses for a repository with no portal: the gate still
/// passes, there is simply nothing to import.
pub async fn seed_directory(
    credential: SeedCredential,
    dir: &Path,
    options: SeedOptions<'_>,
) -> ExitCode {
    run(async {
        if !dir.is_dir() {
            println!("no {} — nothing to import", dir.display());
            return Ok(());
        }
        let documents = seed_documents_in(dir)?;
        if documents.is_empty() {
            eprintln!("no seed documents in {} — nothing to import", dir.display());
            return Ok(());
        }
        let session = resolve_seed_credential(&credential).await?;
        let mut committed = 0usize;
        for (model, path) in documents {
            let yaml = std::fs::read_to_string(&path)
                .with_context(|| format!("read seed file {}", path.display()))?;
            committed += post_seed(&session, model.term(), &yaml, options)
                .await
                .map_err(|failure| failure.into_error(committed))?;
        }
        Ok(())
    })
    .await
}

fn seed_documents_in(dir: &Path) -> Result<Vec<(store::seed::SeedModel, std::path::PathBuf)>> {
    let mut documents = Vec::new();
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("read seed directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read seed directory {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(ext, "yaml" | "yml") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("seed file {} has no UTF-8 stem", path.display()))?;
        let model = store::seed::SeedModel::parse(stem).with_context(|| {
            format!(
                "seed file {} is not a supported `navigator site import` model",
                path.display()
            )
        })?;
        documents.push((model, path));
    }
    documents.sort_by(|(left_model, left_path), (right_model, right_path)| {
        let left = store::seed::SeedModel::ALL
            .iter()
            .position(|model| model == left_model)
            .unwrap_or(usize::MAX);
        let right = store::seed::SeedModel::ALL
            .iter()
            .position(|model| model == right_model)
            .unwrap_or(usize::MAX);
        left.cmp(&right).then_with(|| left_path.cmp(right_path))
    });
    Ok(documents)
}

/// How many records one `POST /app/api/seed` carries. A proxy in front of a
/// deployment can answer `502` while the server goes on to finish a large
/// write; small chunks keep each request well inside its timeout, and a
/// failed one small enough to re-check.
pub(crate) const SEED_CHUNK_RECORDS: usize = 50;

/// The counts a seed reconcile reports, summed over every chunk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) struct SeedCounts {
    #[serde(default)]
    pub(crate) created: usize,
    #[serde(default)]
    pub(crate) updated: usize,
    #[serde(default)]
    pub(crate) unchanged: usize,
}

impl SeedCounts {
    fn add(&mut self, other: Self) {
        self.created += other.created;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
    }
}

/// A seed import that stopped part-way: `committed` records are in place, the
/// rest are not. The `run` wrapper exits `4` for it — see [`exit_code_for`].
#[derive(Debug)]
pub(crate) struct PartialImport {
    pub(crate) committed: usize,
}

impl std::fmt::Display for PartialImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} record(s) were committed before the import stopped",
            self.committed
        )
    }
}

impl std::error::Error for PartialImport {}

/// Why a chunked import stopped, and how much of it had committed.
pub(crate) struct SeedFailure {
    committed: usize,
    error: anyhow::Error,
}

impl SeedFailure {
    /// The error a caller reports, marked [`PartialImport`] when anything —
    /// including `earlier` records from documents before this one — committed.
    pub(crate) fn into_error(self, earlier: usize) -> anyhow::Error {
        let committed = earlier + self.committed;
        if committed == 0 {
            self.error
        } else {
            self.error.context(PartialImport { committed })
        }
    }
}

/// One `POST /app/api/seed` that did not succeed. `status` is `None` when the
/// request never got an answer.
#[derive(Debug)]
struct SeedRefused {
    status: Option<reqwest::StatusCode>,
    message: String,
}

impl SeedRefused {
    /// A gateway or server error, or no answer at all: the write may still
    /// have landed, so the chunk is worth re-checking.
    fn may_have_committed(&self) -> bool {
        self.status.is_none_or(|status| status.is_server_error())
    }
}

impl std::fmt::Display for SeedRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SeedRefused {}

/// Import `records` of one seed `model` through the logged-in deployment, in
/// chunks of [`SEED_CHUNK_RECORDS`]. Never overwrites an existing record.
pub(crate) async fn import_records(
    host: Option<&str>,
    model: &str,
    lookup_fields: &[&str],
    records: &[serde_json::Value],
) -> Result<SeedCounts> {
    let session = resolve(host)?;
    let mut counts = SeedCounts::default();
    let mut committed = 0usize;
    for chunk in records.chunks(SEED_CHUNK_RECORDS) {
        let yaml = seed_document(lookup_fields, chunk)?;
        let (chunk_counts, _) =
            send_chunk(&session, model, &yaml, chunk.len(), SeedOptions::default())
                .await
                .map_err(|failure| failure.into_error(committed))?;
        committed += chunk.len();
        counts.add(chunk_counts);
    }
    Ok(counts)
}

fn seed_document<F: serde::Serialize, R: serde::Serialize>(
    lookup_fields: &[F],
    records: &[R],
) -> Result<String> {
    serde_yaml::to_string(&serde_json::json!({
        "lookup_fields": lookup_fields,
        "records": records,
    }))
    .context("encode seed document")
}

/// The `lookup_fields` and `records` of a seed document, when it has that
/// shape. Anything else is sent whole for the server to refuse by name.
fn seed_records(yaml: &str) -> Option<(Vec<serde_yaml::Value>, Vec<serde_yaml::Value>)> {
    let document: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let lookup_fields = document.get("lookup_fields")?.as_sequence()?.clone();
    let records = document.get("records")?.as_sequence()?.clone();
    Some((lookup_fields, records))
}

/// Submit one seed document, chunked when it carries more than
/// [`SEED_CHUNK_RECORDS`] records, and print each chunk's report. Returns how
/// many records were committed; on failure, how many had been.
async fn post_seed(
    session: &(String, String),
    model: &str,
    yaml: &str,
    options: SeedOptions<'_>,
) -> std::result::Result<usize, SeedFailure> {
    let Some((lookup_fields, records)) =
        seed_records(yaml).filter(|(_, records)| records.len() > SEED_CHUNK_RECORDS)
    else {
        let count = seed_records(yaml).map_or(0, |(_, records)| records.len());
        let (_, body) = send_chunk(session, model, yaml, count, options).await?;
        println!("{body}");
        return Ok(count);
    };
    let total = records.len();
    let mut committed = 0usize;
    for chunk in records.chunks(SEED_CHUNK_RECORDS) {
        let document = seed_document(&lookup_fields, chunk)
            .map_err(|error| SeedFailure { committed, error })?;
        let (_, body) = send_chunk(session, model, &document, chunk.len(), options)
            .await
            .map_err(|failure| {
                println!("committed {committed} of {total} {model} record(s)");
                SeedFailure {
                    committed,
                    error: failure.error,
                }
            })?;
        println!("{body}");
        committed += chunk.len();
    }
    println!("committed {committed} of {total} {model} record(s)");
    Ok(committed)
}

/// Send one chunk. When the answer is lost — a gateway error, a server
/// error, or none at all — re-send it as a dry run: a chunk whose every
/// record now reads unchanged was committed after all.
async fn send_chunk(
    session: &(String, String),
    model: &str,
    yaml: &str,
    records: usize,
    options: SeedOptions<'_>,
) -> std::result::Result<(SeedCounts, String), SeedFailure> {
    let error = match send_seed(session, model, yaml, options).await {
        Ok(body) => return Ok((parse_seed_counts(&body), body)),
        Err(error) => error,
    };
    let may_have_committed = !options.dry_run
        && error
            .downcast_ref::<SeedRefused>()
            .is_some_and(SeedRefused::may_have_committed);
    if may_have_committed {
        if let Ok(body) = send_seed(
            session,
            model,
            yaml,
            SeedOptions {
                dry_run: true,
                ..options
            },
        )
        .await
        {
            let check = parse_seed_counts(&body);
            if records > 0 && check.created == 0 && check.updated == 0 && check.unchanged == records
            {
                eprintln!(
                    "the server answered `{error}`, but a re-check finds all {records} {model} \
                     record(s) committed"
                );
                return Ok((check, body));
            }
        }
    }
    Err(SeedFailure {
        committed: 0,
        error,
    })
}

fn parse_seed_counts(body: &str) -> SeedCounts {
    serde_json::from_str(body).unwrap_or_default()
}

/// `navigator site person merge <keep> <drop>`: one `POST
/// /app/api/people/merge`, printing how many addresses the kept Person answers to (never the
/// parsed `keep`/`drop` arguments, which `CodeQL` treats as secrets).
pub async fn merge_people(host: Option<String>, keep: &str, drop: &str) -> ExitCode {
    run(async {
        let (base, token) = resolve(host.as_deref())?;
        let response = reqwest::Client::new()
            .post(format!("{base}/app/api/people/merge"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "keep": keep, "drop": drop }))
            .send()
            .await
            .context("POST /app/api/people/merge")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| first_line(&body));
            return Err(anyhow!("merge refused: {status}: {message}"));
        }
        let report: serde_json::Value = serde_json::from_str(&body).context("read merge report")?;
        let addresses = report["emails"].as_array().map_or(0, Vec::len);
        println!("merged the two People; the kept Person answers to {addresses} address(es)");
        Ok(())
    })
    .await
}

/// One `POST /app/api/seed`, returning the server's JSON report.
async fn send_seed(
    (base, token): &(String, String),
    model: &str,
    yaml: &str,
    options: SeedOptions<'_>,
) -> Result<String> {
    let mut body = serde_json::json!({
        "model": model,
        "yaml": yaml,
        "overwrite": options.overwrite,
        "dry_run": options.dry_run,
    });
    if let Some(project) = options.as_ci_project {
        body["as_ci"] = serde_json::json!(true);
        body["project"] = serde_json::json!(project);
    }
    let response = reqwest::Client::new()
        .post(format!("{base}/app/api/seed"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            anyhow::Error::new(SeedRefused {
                status: None,
                message: format!("POST /app/api/seed: {error}"),
            })
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::Error::new(SeedRefused {
            status: Some(status),
            message: format!("seed {model} failed: {status}: {}", first_line(&body)),
        }));
    }
    Ok(body)
}

async fn resolve_seed_credential(credential: &SeedCredential) -> Result<(String, String)> {
    match credential {
        SeedCredential::Stored { host } => resolve(host.as_deref()),
        SeedCredential::Ci { host } => resolve_ci(host).await,
    }
}

/// The forge whose Actions runner this process is on. Forgejo's runner also
/// exports the `GITHUB_*` compatibility variables, so `FORGEJO_ACTIONS` is the
/// one signal that tells the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CiForge {
    GitHub,
    Forgejo,
}

impl CiForge {
    /// The forge this runner reports.
    pub(crate) fn detect() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        if get("FORGEJO_ACTIONS").is_some_and(|value| value.trim() == "true") {
            Self::Forgejo
        } else {
            Self::GitHub
        }
    }

    /// The `/auth/ci/*` door that mints a document-scoped session for this
    /// forge's OIDC token.
    pub(crate) fn document_mint_path(self) -> &'static str {
        match self {
            Self::GitHub => "/auth/ci/document-token",
            Self::Forgejo => "/auth/ci/forgejo/document-token",
        }
    }

    /// The `/auth/ci/*` door that mints a seed session for this forge's OIDC
    /// token. The deployment decides write versus dry-run from the run.
    pub(crate) fn seed_mint_path(self) -> &'static str {
        match self {
            Self::GitHub => "/auth/ci/seed-token",
            Self::Forgejo => "/auth/ci/forgejo/seed-token",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub Actions",
            Self::Forgejo => "Forgejo Actions",
        }
    }

    /// The workflow grant that makes the runner export an OIDC request URL.
    fn oidc_grant(self) -> &'static str {
        match self {
            Self::GitHub => "`id-token: write`",
            Self::Forgejo => "`enable-openid-connect: true`",
        }
    }
}

/// Whether this runner can request an OIDC ID token. Forgejo's runner exports
/// both variables empty when the job did not opt in, so empty is unset.
pub(crate) fn ci_oidc_available() -> bool {
    [
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ]
    .iter()
    .all(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

/// Exchange the GitHub Actions OIDC ID token for a project-scoped Navigator
/// seed bearer. The GitHub token is read from the runner environment; the
/// minted Navigator token is held in memory for this process only.
async fn resolve_ci(host: &str) -> Result<(String, String)> {
    resolve_ci_on(host, CiForge::detect()).await
}

/// [`resolve_ci`] on a named forge. Each forge has its own seed door; the
/// deployment, not this process, decides whether the run may write (a push
/// or dispatch on `main`, and on Forgejo only the trusted `cd.yml`) or only
/// dry-run (a pull request).
async fn resolve_ci_on(host: &str, forge: CiForge) -> Result<(String, String)> {
    let base = credentials::base_url(host);
    let client = reqwest::Client::new();
    let oidc_token = actions_oidc_token(&client, &base, forge).await?;
    let minted = mint_ci_token(&client, &base, forge.seed_mint_path(), &oidc_token).await?;
    eprintln!(
        "{}",
        palette::dim(format!(
            "minted a project-scoped seed session for {}",
            minted.project_code
        ))
    );
    Ok((base, minted.token))
}

/// Exchange this run's Actions OIDC ID token for a Navigator session bound to
/// this repository's live Project, for `navigator project gate --check --ci`.
/// GitHub and Forgejo runs each have their own door ([`CiForge`]). The server
/// gives this token only the Project lookup, revision metadata, and
/// document-integrity reads; lawyer-tier attribution never widens that
/// capability.
pub(crate) async fn resolve_ci_document(host: &str) -> Result<(String, String)> {
    resolve_ci_document_on(host, CiForge::detect()).await
}

/// [`resolve_ci_document`] on a named forge.
pub(crate) async fn resolve_ci_document_on(host: &str, forge: CiForge) -> Result<(String, String)> {
    let base = credentials::base_url(host);
    let mint_path = forge.document_mint_path();
    preflight_dns(&base, mint_path)?;
    let client = reqwest::Client::new();
    let oidc_token = actions_oidc_token(&client, &base, forge).await?;
    let minted = mint_ci_token(&client, &base, mint_path, &oidc_token).await?;
    eprintln!(
        "{}",
        palette::dim(format!(
            "minted a project-scoped document session for {}",
            minted.project_code
        ))
    );
    Ok((base, minted.token))
}

/// One gate remote-call attempt, classified for [`retry_gate_call`]. A
/// transient outcome (a connect error, a request timeout, or a 500/502-504
/// status) is retried while attempts remain; a terminal one gives up on the
/// first attempt — every 4xx is a deterministic refusal and is never retried.
pub(crate) enum AttemptOutcome<T> {
    /// The call succeeded.
    Done(T),
    /// Recoverable — retry while attempts remain, then return this error.
    Transient(anyhow::Error),
    /// Final — return this error immediately.
    Terminal(anyhow::Error),
}

/// Runs `attempt` up to `attempts` times, sleeping `delay` scaled by the
/// attempt number before each retry. The posture
/// [`fetch_project_list_with`] established for the gate's idempotent reads,
/// shared by the mint path and the revision reads. The attempt number lets
/// an attempt shape its own final error, so an exhausted call reports the
/// same status, annotation, and exit code it would have carried without the
/// retry.
pub(crate) async fn retry_gate_call<T, F, Fut>(
    attempts: u32,
    delay: std::time::Duration,
    mut attempt: F,
) -> Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = AttemptOutcome<T>>,
{
    for attempt_number in 1..=attempts {
        match attempt(attempt_number).await {
            AttemptOutcome::Done(value) => return Ok(value),
            AttemptOutcome::Terminal(error) => return Err(error),
            AttemptOutcome::Transient(error) if attempt_number == attempts => {
                return Err(error);
            }
            AttemptOutcome::Transient(_) => tokio::time::sleep(delay * attempt_number).await,
        }
    }
    unreachable!("the final attempt either returns or errors");
}

/// Tries per gate mint or OIDC token fetch before the run gives up.
const GATE_MINT_ATTEMPTS: u32 = 4;
/// Wait before the nth retry of a gate mint or OIDC token fetch.
const GATE_MINT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
/// Per-request bound on a gate mint or OIDC token fetch, so a hung endpoint
/// burns an attempt instead of the job's six-hour runner budget.
const GATE_MINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether a response status is safe to repeat — a plain `500` and the
/// gateway errors. Every 4xx is a deterministic refusal and never retried.
pub(crate) fn transient_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 500 | 502..=504)
}

/// Whether a transport error is recoverable — the request never left
/// (connect failure) or gave up on its timeout.
pub(crate) fn transient_transport(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

/// Fetch this Actions run's own OIDC ID token, audienced to `base` — shared
/// by every `/auth/ci/*` mint. GitHub and Forgejo runners export the same two
/// variables and answer with the same `{"value": …}` body.
async fn actions_oidc_token(
    client: &reqwest::Client,
    base: &str,
    forge: CiForge,
) -> Result<String> {
    let runner = forge.name();
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .with_context(|| {
                format!(
                    "{name} is unset — this command runs on {runner} with {}",
                    forge.oidc_grant()
                )
            })
    };
    let request_url = read("ACTIONS_ID_TOKEN_REQUEST_URL")?;
    let request_token = read("ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
    // The token endpoint answers a plain GET, so a transient failure — the
    // runner's token service 5xx-ing or hanging — is retried like every
    // other gate remote call instead of failing the run on one attempt.
    retry_gate_call(GATE_MINT_ATTEMPTS, GATE_MINT_RETRY_DELAY, |attempt| {
        let request = client
            .get(&request_url)
            .query(&[("audience", base)])
            .bearer_auth(&request_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(GATE_MINT_TIMEOUT);
        async move {
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) if transient_transport(&error) => {
                    return AttemptOutcome::Transient(
                        anyhow::Error::new(error).context(format!("request {runner} OIDC token")),
                    );
                }
                Err(error) => {
                    return AttemptOutcome::Terminal(
                        anyhow::Error::new(error).context(format!("request {runner} OIDC token")),
                    );
                }
            };
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                let failure = anyhow!(
                    "{runner} OIDC token request failed: {status}: {}",
                    first_line(&body)
                );
                if transient_status(status) && attempt < GATE_MINT_ATTEMPTS {
                    return AttemptOutcome::Transient(failure);
                }
                return AttemptOutcome::Terminal(failure);
            }
            match serde_json::from_str::<ActionsOidcResponse>(&body) {
                Ok(token) => AttemptOutcome::Done(token.value),
                Err(error) => AttemptOutcome::Terminal(
                    anyhow::Error::new(error)
                        .context(format!("parse {runner} OIDC token response")),
                ),
            }
        }
    })
    .await
}

/// Exchange `github_token` for a Navigator session at one `/auth/ci/*` mint
/// endpoint.
///
/// A non-success response here is the door itself saying no — unauthorized,
/// forbidden, or unavailable — before any gate has run. That is a different
/// kind of failure than a gate that ran and found a problem (exit code `2`),
/// so this prints its own GitHub `::error::` annotation naming the door and
/// the server's message, and marks the error with [`MintRefusal`] so the
/// caller's `run` wrapper exits `3` instead. See cli/README.md.
async fn mint_ci_token(
    client: &reqwest::Client,
    base: &str,
    mint_path: &str,
    github_token: &str,
) -> Result<CiSeedTokenResponse> {
    retry_gate_call(GATE_MINT_ATTEMPTS, GATE_MINT_RETRY_DELAY, |attempt| {
        let request = client
            .post(format!("{base}{mint_path}"))
            .json(&serde_json::json!({ "token": github_token }))
            .timeout(GATE_MINT_TIMEOUT);
        async move {
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    // A DNS miss never got a server response and does not
                    // recover by waiting, so it is terminal like a 4xx
                    // refusal, annotation and all; other transport failures
                    // and timeouts retry like every other gate remote call.
                    let dns = is_dns_failure(&error);
                    let transient = !dns && transient_transport(&error);
                    let mapped = unreachable_host_error(base, &format!("POST {mint_path}"), error);
                    if dns {
                        eprintln!(
                            "{}",
                            mint_refusal_annotation(mint_path, &mapped.to_string())
                        );
                        return AttemptOutcome::Terminal(mapped);
                    }
                    if transient && attempt < GATE_MINT_ATTEMPTS {
                        return AttemptOutcome::Transient(mapped);
                    }
                    return AttemptOutcome::Terminal(mapped);
                }
            };
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if status.is_success() {
                return match serde_json::from_str(&body) {
                    Ok(minted) => AttemptOutcome::Done(minted),
                    Err(error) => AttemptOutcome::Terminal(
                        anyhow::Error::new(error).context(format!("parse {mint_path} response")),
                    ),
                };
            }
            let message = mint_refusal_message(&body);
            if transient_status(status) && attempt < GATE_MINT_ATTEMPTS {
                return AttemptOutcome::Transient(anyhow!(
                    "CI token mint at {mint_path} failed: {status}: {message}"
                ));
            }
            eprintln!(
                "{}",
                mint_refusal_annotation(mint_path, &format!("{status}: {message}"))
            );
            AttemptOutcome::Terminal(anyhow::Error::new(MintRefusal).context(format!(
                "CI token mint at {mint_path} failed: {status}: {message}"
            )))
        }
    })
    .await
}

/// Resolve the serving host before any request leaves the runner, so a host
/// with no DNS record fails at the earliest step — before the OIDC audience
/// request — with the host-naming remedies and the CI annotation instead of a
/// raw resolver error mid-mint. A lookup failure that is not a name miss is
/// left for the real request to report.
fn preflight_dns(base: &str, mint_path: &str) -> Result<()> {
    let Some((host, port)) = dns_target(base) else {
        return Ok(());
    };
    match (host.as_str(), port).to_socket_addrs() {
        Err(error) if error_chain_names_dns(&error) => {
            let request = format!("POST {mint_path}");
            let message = unreachable_host_message(base, &request);
            eprintln!("{}", mint_refusal_annotation(mint_path, &message));
            Err(anyhow!(message))
        }
        // Resolved, or a lookup failure that is not a name miss: the real
        // request reports the latter in its own words.
        _ => Ok(()),
    }
}

/// The name and port a pre-flight lookup asks for, from a base URL.
fn dns_target(base: &str) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(base).ok()?;
    let host = url.host_str()?.to_string();
    Some((host, url.port_or_known_default().unwrap_or(443)))
}

/// The error a request reports when it never reached Navigator — the mint
/// POST and the gate's projects fetch share it. A host with no DNS record (a
/// Brand host not yet published, say) is named as such, so the run says which
/// `project.host` to fix rather than surfacing a raw resolver error; any
/// other transport failure still names the host.
pub(crate) fn unreachable_host_error(
    base: &str,
    request: &str,
    error: reqwest::Error,
) -> anyhow::Error {
    let message = if is_dns_failure(&error) {
        unreachable_host_message(base, request)
    } else {
        format!("{request} could not reach project.host `{}`", host_of(base))
    };
    anyhow::Error::new(error).context(message)
}

/// The host-naming message every DNS-refused request shares: the host, the
/// fact nothing was reached, and both remedies — publish the record, or point
/// `project.host` at a host this deployment serves.
fn unreachable_host_message(base: &str, request: &str) -> String {
    format!(
        "project.host `{}` does not resolve: no DNS record answers for it, so \
         {request} never reached Navigator. Publish the host's DNS record, or set \
         project.host to a host this deployment serves.",
        host_of(base)
    )
}

/// The host part of a base URL, for naming it in refusals.
fn host_of(base: &str) -> &str {
    base.split_once("://")
        .map_or(base, |(_, rest)| rest)
        .trim_end_matches('/')
}

/// Whether a transport error is a name-resolution failure, judged by its
/// source chain: reqwest reports a resolver failure as a connect error whose
/// cause reads `dns error`, with the platform resolver's own words beneath.
fn is_dns_failure(error: &reqwest::Error) -> bool {
    error_chain_names_dns(error)
}

/// Whether an error's source chain reads as a name-resolution failure — the
/// needles cover both the wording under a reqwest connect error and the words
/// an `std::io::Error` from a direct lookup carries.
fn error_chain_names_dns(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        let text = current.to_string().to_ascii_lowercase();
        if [
            "dns error",
            "failed to lookup address",
            "nodename nor servname",
            "name or service not known",
            "no such host",
        ]
        .iter()
        .any(|needle| text.contains(needle))
        {
            return true;
        }
        source = current.source();
    }
    false
}

/// The GitHub `::error::` workflow-command annotation `mint_ci_token` and the
/// DNS pre-flight print for a refused mint, naming the door (`mint_path`) and
/// the reason — a server status with its message, or a host that does not
/// resolve — so a repository's Actions log shows *why* the run was refused
/// rather than a generic HTTP status.
fn mint_refusal_annotation(mint_path: &str, reason: &str) -> String {
    format!("::error::CI token mint at {mint_path} was refused: {reason}")
}

/// A CI token mint refused by one of the `/auth/ci/*` doors
/// itself. `mint_ci_token` has already printed the GitHub `::error::`
/// annotation; this marker only carries which exit code the caller's `run`
/// wrapper should choose — see [`exit_code_for`].
#[derive(Debug)]
pub(crate) struct MintRefusal;

impl std::fmt::Display for MintRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CI token mint was refused")
    }
}

impl std::error::Error for MintRefusal {}

/// The exit code a `run` wrapper should return for `error`: `3` when the
/// chain carries a [`MintRefusal`] (the CI door refused the mint itself),
/// `4` when a seed import stopped after committing part of its records
/// ([`PartialImport`]), and `2` for every other failure — the ordinary
/// gate-failure code. Documented in cli/README.md.
pub(crate) fn exit_code_for(error: &anyhow::Error) -> ExitCode {
    if error
        .chain()
        .any(<dyn std::error::Error>::is::<MintRefusal>)
    {
        ExitCode::from(3)
    } else if error.downcast_ref::<PartialImport>().is_some() {
        ExitCode::from(4)
    } else {
        ExitCode::from(2)
    }
}

/// The door's own `message` field when the refusal body is the JSON
/// `{"error", "message"}` shape `ci_auth::MintError` renders; the raw first
/// line otherwise, so a refusal from something other than that door (a proxy,
/// a timeout page) still surfaces as text rather than an empty annotation.
fn mint_refusal_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct RefusalBody {
        message: String,
    }
    serde_json::from_str::<RefusalBody>(body)
        .map_or_else(|_| first_line(body), |refusal| refusal.message)
}

#[derive(Debug, Deserialize)]
struct ActionsOidcResponse {
    value: String,
}

#[derive(Debug, Deserialize)]
struct CiSeedTokenResponse {
    token: String,
    project_code: String,
}

#[derive(Debug, Deserialize)]
struct VisibleProject {
    id: Uuid,
    code: String,
}

/// One revision as reported by `GET
/// /app/api/projects/{id}/documents/revisions?slug=` — what `navigator
/// document log`/`get`/`diff` (#485) read.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RevisionSummary {
    pub(crate) version: usize,
    pub(crate) asset_id: Uuid,
    pub(crate) created_at: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: i64,
    pub(crate) filename: String,
    #[serde(default)]
    pub(crate) visibility: String,
    pub(crate) operative: bool,
    /// When this revision was soft-deleted. Only a lawyer-lens caller reads
    /// a soft-deleted chain at all, and the door omits the field on every
    /// live row; the gate uses it to keep a soft-deleted revision out of
    /// the chain-extension tolerance (LAW-284).
    #[serde(default)]
    pub(crate) deleted_at: Option<String>,
    #[serde(default)]
    pub(crate) signing: Option<store::documents::Signing>,
    #[serde(default)]
    pub(crate) notarization: Vec<store::documents::Notarization>,
}

/// Rows one integrity request asks for. Each row costs the server a storage
/// read, so a page stays well inside the gateway's timeout.
const INTEGRITY_PAGE: usize = 50;
/// Rows per request when the server re-hashes each object.
const INTEGRITY_DEEP_PAGE: usize = 10;
/// Tries per page or per revision read before the check fails. The reads
/// share one budget: both are bounded idempotent GETs, and both retry a
/// plain 500 alongside the gateway errors.
const INTEGRITY_ATTEMPTS: u32 = 3;
const INTEGRITY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(100);
const INTEGRITY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// One asset row from `GET /app/api/projects/{id}/documents/integrity`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IntegrityAsset {
    pub(crate) asset_id: Uuid,
    pub(crate) slug: Option<String>,
    #[serde(default)]
    pub(crate) sha256: String,
    #[serde(default)]
    pub(crate) kind: Option<String>,
    #[serde(default)]
    pub(crate) derived_from: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) operative: bool,
    #[serde(default)]
    pub(crate) version: Option<usize>,
    pub(crate) exists: bool,
    #[serde(default)]
    pub(crate) size_bytes: Option<i64>,
    pub(crate) recorded_size: i64,
    #[serde(default)]
    pub(crate) sha256_matches: Option<bool>,
    #[serde(default)]
    pub(crate) docket_name_matches: Option<bool>,
}

/// One server-side external-id finding. Empty until a pointer carries an
/// external id.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IntegrationFinding {
    pub(crate) asset_id: Uuid,
    pub(crate) integration: String,
    pub(crate) outcome: String,
    pub(crate) detail: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IntegrityResponse {
    pub(crate) assets: Vec<IntegrityAsset>,
    /// Rows across every page. Absent from a server that does not page.
    #[serde(default)]
    pub(crate) total: Option<usize>,
    /// Where the next page starts. Absent on the last page.
    #[serde(default)]
    pub(crate) next_offset: Option<usize>,
    #[serde(default)]
    pub(crate) integrations: Vec<IntegrationFinding>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RevisionsResponse {
    pub(crate) kind: String,
    /// Newest first, matching `store::assets::revisions`.
    pub(crate) revisions: Vec<RevisionSummary>,
}

/// One filed document revision returned by the Project inventory door. The
/// server applies the caller's matter lens before this shape reaches the CLI.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LiveDocumentSummary {
    #[serde(default)]
    pub(crate) metadata: Option<serde_json::Value>,
    pub(crate) id: Uuid,
    pub(crate) byte_size: i64,
    pub(crate) sha256_hex: String,
    pub(crate) filename: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) visibility: String,
    pub(crate) slug: Option<String>,
    pub(crate) inserted_at: String,
    #[serde(default)]
    pub(crate) signing: Option<store::documents::Signing>,
    #[serde(default)]
    pub(crate) notarization: Vec<store::documents::Notarization>,
}

/// How an executed document was executed, as an upload records it. Empty for
/// every upload that is not an executed document.
#[derive(Debug, Clone, Default)]
pub(crate) struct DocumentExecution {
    pub(crate) signing: Option<store::documents::Signing>,
    pub(crate) notarization: Vec<store::documents::Notarization>,
    pub(crate) evidence: Option<store::documents::Evidence>,
}

impl DocumentExecution {
    fn add_to(&self, body: &mut serde_json::Value) -> Result<()> {
        if let Some(signing) = self.signing {
            body["signing"] = serde_json::to_value(signing).context("encode signing")?;
        }
        if !self.notarization.is_empty() {
            body["notarization"] =
                serde_json::to_value(&self.notarization).context("encode notarization")?;
        }
        if let Some(evidence) = &self.evidence {
            body["evidence"] = serde_json::to_value(evidence).context("encode evidence")?;
        }
        Ok(())
    }
}

/// Which upload door carries a document: the buffered door for everything
/// that fits its per-kind cap, the streaming lane above it (LAW-172).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadLane {
    Buffered,
    Streaming,
}

/// The lane for a document of `size` bytes and `kind`. Inclusive at the
/// per-kind cap, so every upload that works today stays on the buffered
/// door and only the previously rejected sizes take the streaming lane.
pub(crate) fn upload_lane_for(size: usize, kind: &str) -> UploadLane {
    if size <= store::documents::max_document_upload_bytes(kind) {
        UploadLane::Buffered
    } else {
        UploadLane::Streaming
    }
}

/// The wire shape of a streaming upload session: where the upload stands
/// and what the next chunk must carry. The id is parsed from the same
/// response.
#[derive(Debug, Clone, Deserialize)]
struct StreamingSessionStatus {
    id: Uuid,
    chunk_size: u64,
    received_bytes: u64,
}

/// The deployment answered `401 Unauthorized`: the session expired or never
/// authenticated. It says nothing about whether a document exists, so a
/// caller reports it as a session failure, never as a missing document.
#[derive(Debug)]
pub(crate) struct SessionRefused {
    pub(crate) detail: String,
}

impl std::fmt::Display for SessionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the deployment refused the session (401 Unauthorized)")?;
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for SessionRefused {}

/// Whether `error` is a [`SessionRefused`].
pub(crate) fn is_session_refused(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SessionRefused>().is_some()
}

/// Authenticated client for the one Project's document-sync operations.
#[derive(Debug)]
pub(crate) struct DocumentClient {
    base: String,
    token: String,
    client: reqwest::Client,
    project_id: Uuid,
    project_code: String,
}

/// Build an authenticated transfer transport for the selected deployment.
/// Remote endpoints require HTTPS; loopback HTTP supports the local fixture.
/// Redirects stay disabled so credentials and payloads remain on that endpoint.
///
/// # Errors
///
/// Rejects invalid URLs and non-loopback plaintext origins.
pub(crate) fn authenticated_client_builder(base: &str) -> Result<reqwest::ClientBuilder> {
    let origin = url::Url::parse(base).context("parse authenticated deployment URL")?;
    let loopback = match origin.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain("localhost")) => true,
        _ => false,
    };
    anyhow::ensure!(
        origin.scheme() == "https" || (origin.scheme() == "http" && loopback),
        "authenticated transfers require HTTPS except for a loopback deployment"
    );
    Ok(reqwest::Client::builder()
        .https_only(!loopback)
        .redirect(reqwest::redirect::Policy::none()))
}

impl DocumentClient {
    /// The deployment this client is logged in to.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// The bearer token the login resolved, for a sibling door on the same
    /// deployment and Project.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The Project the client files into.
    pub(crate) fn project_id(&self) -> Uuid {
        self.project_id
    }

    /// Resolve a logged-in host and one visible Project code.
    pub(crate) async fn connect(host: Option<&str>, project_code: &str) -> Result<Self> {
        let (base, token) = resolve(host)?;
        Self::with_credential(base, token, project_code).await
    }

    /// Build a client from an already-minted `(base, token)` pair — the
    /// `navigator project gate --check --ci` path, which authenticates via
    /// GitHub Actions OIDC rather than a stored `~/.navigator.json` login.
    /// Its requests carry the integrity client's 100 s bound, so a hung
    /// gateway burns an attempt instead of the job's six-hour budget.
    pub(crate) async fn with_credential(
        base: String,
        token: String,
        project_code: &str,
    ) -> Result<Self> {
        Self::with_timeout(base, token, project_code, Some(INTEGRITY_REQUEST_TIMEOUT)).await
    }

    /// Bounded, read-only validation requests. The normal client keeps its
    /// existing transfer policy for document uploads and downloads.
    pub(crate) async fn for_validation(
        base: String,
        token: String,
        project_code: &str,
    ) -> Result<Self> {
        Self::with_timeout(
            base,
            token,
            project_code,
            Some(std::time::Duration::from_secs(10)),
        )
        .await
    }

    async fn with_timeout(
        base: String,
        token: String,
        project_code: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<Self> {
        let mut builder = authenticated_client_builder(&base)?;
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        let client = builder.build().context("build document transfer client")?;
        let body = fetch_project_list(&client, &base, &token).await?;
        let projects: Vec<VisibleProject> =
            serde_json::from_str(&body).context("parse GET /app/api/projects")?;
        let project_id = projects
            .iter()
            .find(|project| project.code == project_code)
            .map(|project| project.id)
            .ok_or_else(|| anyhow!("no visible matter with code `{project_code}`"))?;
        Ok(Self {
            base,
            token,
            client,
            project_id,
            project_code: project_code.to_string(),
        })
    }

    /// The revision chain of `slug` under the caller's own lens
    /// (`GET /app/api/projects/{id}/documents/revisions?slug=`).
    ///
    /// The read is idempotent, so a transient failure — a 500, a gateway
    /// 502-504, a timeout, a dropped connection — is retried before one
    /// slug's read becomes a `CheckFailure`. A 401 is never retried here:
    /// it means the session lapsed, and renewal above this layer (`LiveSession`)
    /// stays the one path that answers it.
    pub(crate) async fn list_revisions(&self, slug: &str) -> Result<RevisionsResponse> {
        let url = format!(
            "{}/app/api/projects/{}/documents/revisions",
            self.base, self.project_id
        );
        retry_gate_call(INTEGRITY_ATTEMPTS, INTEGRITY_RETRY_DELAY, |attempt| {
            let url = url.clone();
            let request = self
                .client
                .get(&url)
                .query(&[("slug", slug)])
                .bearer_auth(&self.token)
                .timeout(INTEGRITY_REQUEST_TIMEOUT);
            async move {
                let response = match request.send().await {
                    Ok(response) => response,
                    Err(error) if transient_transport(&error) => {
                        return AttemptOutcome::Transient(
                            anyhow::Error::new(error).context(format!("GET {url}")),
                        );
                    }
                    Err(error) => {
                        return AttemptOutcome::Terminal(
                            anyhow::Error::new(error).context(format!("GET {url}")),
                        );
                    }
                };
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    return AttemptOutcome::Terminal(anyhow::Error::new(SessionRefused {
                        detail: first_line(&text),
                    }));
                }
                if status.is_success() {
                    return match serde_json::from_str(&text) {
                        Ok(revisions) => AttemptOutcome::Done(revisions),
                        Err(error) => AttemptOutcome::Terminal(
                            anyhow::Error::new(error).context("parse document revisions response"),
                        ),
                    };
                }
                let failure = anyhow!(
                    "no revision of `{slug}` is visible on this matter: {status}: {}",
                    first_line(&text)
                );
                if transient_status(status) && attempt < INTEGRITY_ATTEMPTS {
                    AttemptOutcome::Transient(failure)
                } else {
                    AttemptOutcome::Terminal(failure)
                }
            }
        })
        .await
    }

    /// The same deployment, Project, and transport with a freshly minted
    /// bearer — the gate's renewal after a CI session lapses mid-run.
    pub(crate) fn with_token(&self, token: String) -> Self {
        Self {
            base: self.base.clone(),
            token,
            client: self.client.clone(),
            project_id: self.project_id,
            project_code: self.project_code.clone(),
        }
    }

    /// Every asset row the caller's lens can see, with storage presence and
    /// size. `deep` asks the server to re-hash each object.
    ///
    /// The server answers in bounded pages, so a matter with hundreds of
    /// pointers never waits on one gateway-length request. A page that fails
    /// at the gateway or times out is retried; the rest of the pages are
    /// never skipped, so every row is still checked.
    pub(crate) async fn integrity(&self, deep: bool) -> Result<IntegrityResponse> {
        let limit = if deep {
            INTEGRITY_DEEP_PAGE
        } else {
            INTEGRITY_PAGE
        };
        let mut merged = IntegrityResponse {
            assets: Vec::new(),
            total: None,
            next_offset: None,
            integrations: Vec::new(),
        };
        let mut offset = 0;
        loop {
            let page = self.integrity_page(deep, offset, limit).await?;
            merged.total = page.total;
            merged.assets.extend(page.assets);
            merged.integrations.extend(page.integrations);
            match page.next_offset {
                None => return Ok(merged),
                Some(next) if next > offset => offset = next,
                Some(next) => {
                    return Err(anyhow!(
                        "document integrity paging did not advance: the server answered \
                         next_offset {next} for offset {offset}"
                    ));
                }
            }
        }
    }

    async fn integrity_page(
        &self,
        deep: bool,
        offset: usize,
        limit: usize,
    ) -> Result<IntegrityResponse> {
        let url = format!(
            "{}/app/api/projects/{}/documents/integrity",
            self.base, self.project_id
        );
        let started = std::time::Instant::now();
        for attempt in 1..=INTEGRITY_ATTEMPTS {
            let failure;
            let mut request = self
                .client
                .get(&url)
                .bearer_auth(&self.token)
                .timeout(INTEGRITY_REQUEST_TIMEOUT)
                .query(&[("offset", offset), ("limit", limit)]);
            if deep {
                request = request.query(&[("deep", "true")]);
            }
            let retryable = match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if status.is_success() {
                        return serde_json::from_str(&text).with_context(|| {
                            format!("parse document integrity response at offset {offset}")
                        });
                    }
                    failure = integrity_failure(Ok((status, &text)), INTEGRITY_REQUEST_TIMEOUT);
                    transient_status(status)
                }
                Err(error) => {
                    failure = integrity_failure(Err(&error), INTEGRITY_REQUEST_TIMEOUT);
                    transient_transport(&error)
                }
            };
            if !retryable || attempt == INTEGRITY_ATTEMPTS {
                return Err(anyhow!(
                    "document integrity failed: {failure} (rows from offset {offset}, \
                     {limit} per page{}, attempt {attempt} of {INTEGRITY_ATTEMPTS}, \
                     {}s elapsed)",
                    if deep { ", deep" } else { "" },
                    started.elapsed().as_secs()
                ));
            }
            tokio::time::sleep(INTEGRITY_RETRY_DELAY * attempt).await;
        }
        unreachable!("the final attempt returns")
    }

    /// Every document revision visible through the caller's Project lens.
    pub(crate) async fn list_documents(&self) -> Result<Vec<LiveDocumentSummary>> {
        let url = format!(
            "{}/app/api/projects/{}/documents",
            self.base, self.project_id
        );
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "list documents failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document inventory response")
    }

    /// `GET /app/api/projects/{id}/service` — every Service on the Project.
    pub(crate) async fn list_service(&self) -> Result<Vec<store::project_service::ProjectService>> {
        let url = format!("{}/app/api/projects/{}/service", self.base, self.project_id);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "list service failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse service response")
    }

    /// `PUT /app/api/projects/{id}/pages` — replace the Project's stored
    /// Matter Pages with `pages`. Returns how many the server stored.
    pub(crate) async fn put_pages(&self, pages: &[rules::pages::PageEntry]) -> Result<usize> {
        let url = format!("{}/app/api/projects/{}/pages", self.base, self.project_id);
        let response = self
            .client
            .put(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "pages": pages }))
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "store matter pages failed: {status}: {}",
                first_line(&text)
            ));
        }
        let body: serde_json::Value =
            serde_json::from_str(&text).context("parse matter pages response")?;
        Ok(body["pages"].as_array().map_or(0, Vec::len))
    }

    /// `PUT /app/api/projects/{id}/matter` — replace the Project's stored
    /// Matter Caption with `matter`; `None` removes it. Returns whether the
    /// server now holds one.
    pub(crate) async fn put_matter(&self, matter: Option<&word::matter::Matter>) -> Result<bool> {
        let url = format!("{}/app/api/projects/{}/matter", self.base, self.project_id);
        let response = self
            .client
            .put(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "matter": matter }))
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "store matter caption failed: {status}: {}",
                first_line(&text)
            ));
        }
        let body: serde_json::Value =
            serde_json::from_str(&text).context("parse matter caption response")?;
        Ok(!body["matter"].is_null())
    }

    /// A client aimed at a test server, with no login lookup.
    #[cfg(test)]
    pub(crate) fn for_check_tests(base: impl Into<String>, project_id: Uuid) -> Self {
        Self {
            base: base.into(),
            token: "test-token".into(),
            client: reqwest::Client::new(),
            project_id,
            project_code: "acme".into(),
        }
    }

    /// Fetch one revision's bytes through the existing Project-scoped download
    /// route — the same cross-project and caller-lens guards the browser's
    /// download link applies. `reqwest`'s default client follows the redirect
    /// to a signed storage URL (production) transparently; `FsStorage`
    /// (local dev) streams bytes directly from the same route.
    pub(crate) async fn download_revision(&self, asset_id: Uuid) -> Result<Vec<u8>> {
        let url = format!(
            "{}/app/projects/{}/documents/{asset_id}/download",
            self.base, self.project_code
        );
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "download of revision {asset_id} failed: {status}: {}",
                first_line(&text)
            ));
        }
        Ok(response
            .bytes()
            .await
            .context("read revision bytes")?
            .to_vec())
    }

    /// Fetch an Authority's archived bytes through its read door
    /// (`GET /app/api/authorities/{id}/archive`), gated like the write.
    pub(crate) async fn download_authority_archive(&self, authority_id: Uuid) -> Result<Vec<u8>> {
        let url = format!("{}/app/api/authorities/{authority_id}/archive", self.base);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "download of authority {authority_id} failed: {status}: {}",
                first_line(&text)
            ));
        }
        Ok(response
            .bytes()
            .await
            .context("read authority archive bytes")?
            .to_vec())
    }

    /// Upload one revision from bytes already in memory — the archive
    /// command's own path, which has no file on disk to read (ENG-481).
    /// `metadata`, when present, is passed through to the asset row's own
    /// `metadata` column verbatim (a source repository's commit SHA, for
    /// instance); the content hash needs no separate field, since the
    /// server derives it from the bytes themselves.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upload_bytes(
        &self,
        filename: &str,
        bytes: &[u8],
        kind: &str,
        visibility: Option<&str>,
        description: Option<&str>,
        content_type: Option<&str>,
        slug: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<store::document_pointers::DocumentPointer> {
        self.upload_bytes_with_details(
            filename,
            bytes,
            kind,
            visibility,
            description,
            content_type,
            slug,
            metadata,
            None,
            None,
            &DocumentExecution::default(),
        )
        .await
    }

    /// Upload a revision with the structured OCR provenance fields. Routes
    /// by size: the buffered door for everything that fits its per-kind cap,
    /// the streaming lane above it (LAW-172) — one lane choice, made here,
    /// so no upload command needs to know the difference.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upload_bytes_with_details(
        &self,
        filename: &str,
        bytes: &[u8],
        kind: &str,
        visibility: Option<&str>,
        description: Option<&str>,
        content_type: Option<&str>,
        slug: Option<&str>,
        metadata: Option<serde_json::Value>,
        derived_from: Option<serde_json::Value>,
        transcript_quality: Option<&str>,
        execution: &DocumentExecution,
    ) -> Result<store::document_pointers::DocumentPointer> {
        match upload_lane_for(bytes.len(), kind) {
            UploadLane::Buffered => {
                self.upload_bytes_buffered(
                    filename,
                    bytes,
                    kind,
                    visibility,
                    description,
                    content_type,
                    slug,
                    metadata,
                    derived_from,
                    transcript_quality,
                    execution,
                )
                .await
            }
            UploadLane::Streaming => {
                if bytes.len() > store::documents::MAX_STREAMING_UPLOAD_BYTES {
                    return Err(anyhow!(
                        "{}",
                        store::documents::streaming_upload_size_message(bytes.len())
                    ));
                }
                self.upload_bytes_streaming(
                    filename,
                    bytes,
                    kind,
                    visibility,
                    description,
                    content_type,
                    slug,
                    metadata,
                    derived_from,
                    transcript_quality,
                    execution,
                )
                .await
            }
        }
    }

    /// The buffered upload: one base64 JSON request — the lane every upload
    /// fit until the streaming lane existed, unchanged.
    #[allow(clippy::too_many_arguments)]
    async fn upload_bytes_buffered(
        &self,
        filename: &str,
        bytes: &[u8],
        kind: &str,
        visibility: Option<&str>,
        description: Option<&str>,
        content_type: Option<&str>,
        slug: Option<&str>,
        metadata: Option<serde_json::Value>,
        derived_from: Option<serde_json::Value>,
        transcript_quality: Option<&str>,
        execution: &DocumentExecution,
    ) -> Result<store::document_pointers::DocumentPointer> {
        validate_document_upload_size_in_bytes(kind, bytes.len())?;
        let effective_slug = slug
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(filename);
        store::documents::validate_document_slug(filename, effective_slug)
            .map_err(anyhow::Error::from)?;
        let mut body = serde_json::json!({
            "filename": filename,
            "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "content_type": content_type.unwrap_or("application/octet-stream"),
            "kind": kind,
            "visibility": visibility.unwrap_or("internal"),
        });
        if let Some(description) = description.map(str::trim).filter(|value| !value.is_empty()) {
            body["description"] = serde_json::Value::String(description.to_string());
        }
        if let Some(slug) = slug.map(str::trim).filter(|value| !value.is_empty()) {
            body["slug"] = serde_json::Value::String(slug.to_string());
        }
        if let Some(metadata) = metadata {
            body["metadata"] = metadata;
        }
        if let Some(derived_from) = derived_from {
            body["derived_from"] = derived_from;
        }
        if let Some(transcript_quality) = transcript_quality {
            body["transcript_quality"] = serde_json::Value::String(transcript_quality.to_string());
        }
        execution.add_to(&mut body)?;
        let url = format!(
            "{}/app/api/projects/{}/documents",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}: upload outcome unknown; reconcile with `navigator project gate --check` before retrying"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(UploadSessionExpired(self.base.clone()).into());
        }
        if !matches!(status.as_u16(), 200 | 201) {
            return Err(anyhow!(
                "document upload failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document pointer response: upload may be filed; reconcile with `navigator project gate --check` before retrying")
    }

    /// Upload one revision through the streaming lane (LAW-172): open (or
    /// resume) a session, send the bytes one server-sized chunk per call at
    /// exactly the offset the server acknowledges, and complete. Resumable
    /// by construction — re-running the command re-opens the same session
    /// (same filename, size, and digest), re-syncs from the acknowledged
    /// offset, and never re-sends a byte the server already counted. Files
    /// through the same ingest as the buffered door, so the pointer and its
    /// revisions are identical.
    #[allow(clippy::too_many_arguments)]
    async fn upload_bytes_streaming(
        &self,
        filename: &str,
        bytes: &[u8],
        kind: &str,
        visibility: Option<&str>,
        description: Option<&str>,
        content_type: Option<&str>,
        slug: Option<&str>,
        metadata: Option<serde_json::Value>,
        derived_from: Option<serde_json::Value>,
        transcript_quality: Option<&str>,
        execution: &DocumentExecution,
    ) -> Result<store::document_pointers::DocumentPointer> {
        let mut body = serde_json::json!({
            "filename": filename,
            "content_type": content_type.unwrap_or("application/octet-stream"),
            "kind": kind,
            "visibility": visibility.unwrap_or("internal"),
            "declared_size": bytes.len(),
            "declared_sha256": store::documents::sha256_hex(bytes),
        });
        if let Some(description) = description.map(str::trim).filter(|value| !value.is_empty()) {
            body["description"] = serde_json::Value::String(description.to_string());
        }
        if let Some(slug) = slug.map(str::trim).filter(|value| !value.is_empty()) {
            body["slug"] = serde_json::Value::String(slug.to_string());
        }
        if let Some(metadata) = metadata {
            body["metadata"] = metadata;
        }
        if let Some(derived_from) = derived_from {
            body["derived_from"] = derived_from;
        }
        if let Some(transcript_quality) = transcript_quality {
            body["transcript_quality"] = serde_json::Value::String(transcript_quality.to_string());
        }
        execution.add_to(&mut body)?;
        let session_url = format!(
            "{}/app/api/projects/{}/documents/upload-session",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&session_url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {session_url}: session open outcome unknown"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(UploadSessionExpired(self.base.clone()).into());
        }
        if !matches!(status.as_u16(), 200 | 201) {
            return Err(anyhow!(
                "streaming upload failed to open a session: {status}: {}",
                first_line(&text)
            ));
        }
        let session: StreamingSessionStatus =
            serde_json::from_str(&text).context("parse upload session response")?;
        let session_id = session.id;
        self.stream_upload_chunks(bytes, session).await?;

        let complete_url = format!(
            "{}/app/api/projects/{}/documents/upload-session/complete",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&complete_url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "session_id": session_id }))
            .send()
            .await
            .with_context(|| {
                format!(
                    "POST {complete_url}: completion outcome unknown; re-running \
                     the command resumes from the acknowledged offset"
                )
            })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(UploadSessionExpired(self.base.clone()).into());
        }
        if !matches!(status.as_u16(), 200 | 201) {
            return Err(anyhow!(
                "streaming upload completion failed: {status}: {} — the session \
                 kept its progress; re-run the command to retry",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context(
            "parse document pointer response: upload may be filed; reconcile with \
             `navigator project gate --check` before retrying",
        )
    }

    async fn stream_upload_chunks(
        &self,
        bytes: &[u8],
        mut session: StreamingSessionStatus,
    ) -> Result<()> {
        let session_id = session.id;
        let mut offset = usize::try_from(session.received_bytes)
            .context("upload offset exceeds addressable memory")?;

        // A 409 means the server stands at a different offset than this run
        // assumed — a lost acknowledgement, or a chunk sized for a stale
        // session view. Re-sync from the status door and continue; after a
        // few consecutive refusals the mismatch is real, so fail with the
        // offset named instead of looping.
        let mut resyncs = 0_u32;
        while offset < bytes.len() {
            let chunk_len = usize::try_from(session.chunk_size.max(1))
                .context("upload chunk size exceeds addressable memory")?;
            let end = offset.saturating_add(chunk_len).min(bytes.len());
            let chunk = &bytes[offset..end];
            let chunk_url = format!(
                "{}/app/api/projects/{}/documents/upload-session/chunk",
                self.base, self.project_id
            );
            let response = self
                .client
                .post(&chunk_url)
                .bearer_auth(&self.token)
                .json(&serde_json::json!({
                    "session_id": session_id,
                    "offset": offset,
                    "content_base64":
                        base64::engine::general_purpose::STANDARD.encode(chunk),
                }))
                .send()
                .await
                .with_context(|| {
                    format!(
                        "POST {chunk_url}: chunk {offset} outcome unknown; \
                         re-running the command resumes from the acknowledged offset"
                    )
                })?;
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(UploadSessionExpired(self.base.clone()).into());
            }
            if status == reqwest::StatusCode::CONFLICT {
                resyncs += 1;
                if resyncs > 4 {
                    return Err(anyhow!(
                        "the server refused the chunk at offset {offset} repeatedly; \
                         re-run the command to resume from the acknowledged offset"
                    ));
                }
                session = self.upload_session_status(session_id).await?;
                offset = usize::try_from(session.received_bytes)
                    .context("upload offset exceeds addressable memory")?;
                continue;
            }
            if status == reqwest::StatusCode::GONE {
                return Err(anyhow!(
                    "the upload session expired mid-upload; re-run the command \
                     to open a new one (progress is not recoverable)"
                ));
            }
            if !status.is_success() {
                return Err(anyhow!(
                    "chunk at offset {offset} failed: {status}: {} — re-run the \
                     command to resume from the acknowledged offset",
                    first_line(&text)
                ));
            }
            let ack: StreamingSessionStatus =
                serde_json::from_str(&text).context("parse upload session acknowledgement")?;
            let acknowledged = usize::try_from(ack.received_bytes)
                .context("upload acknowledgement exceeds addressable memory")?;
            if acknowledged <= offset && end < bytes.len() {
                return Err(anyhow!(
                    "the server acknowledged {} bytes after the chunk at offset \
                     {offset}; re-run the command to resume",
                    ack.received_bytes
                ));
            }
            offset = acknowledged;
        }

        Ok(())
    }

    /// Read one streaming upload session's status — the resume point and the
    /// chunk size the server is enforcing.
    async fn upload_session_status(&self, session_id: Uuid) -> Result<StreamingSessionStatus> {
        let url = format!(
            "{}/app/api/projects/{}/documents/upload-session/status",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "session_id": session_id }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!(
                "the streaming upload session is gone (completed or expired); \
                 re-run the command to open a new one"
            ));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(UploadSessionExpired(self.base.clone()).into());
        }
        if !status.is_success() {
            return Err(anyhow!(
                "upload session status failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse upload session status")
    }

    /// Reconcile the desired pointer visibility through Navigator's API.
    pub(crate) async fn set_visibility(&self, asset_id: Uuid, visibility: &str) -> Result<()> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}",
            self.base, self.project_id
        );
        let response = self
            .client
            .patch(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "visibility": visibility }))
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document visibility failed: {status}: {}",
                first_line(&text)
            ));
        }
        Ok(())
    }

    /// Set `slug` on a row that has none. `kind`, when set, is written in the
    /// same request. `dry_run` asks the server to validate and change nothing.
    pub(crate) async fn assign_slug(
        &self,
        asset_id: Uuid,
        slug: &str,
        kind: Option<&str>,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}",
            self.base, self.project_id
        );
        let mut body = serde_json::json!({ "slug": slug, "dry_run": dry_run });
        if let Some(kind) = kind {
            body["kind"] = serde_json::Value::String(kind.to_string());
        }
        let response = self
            .client
            .patch(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document slug failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document slug response")
    }

    /// Replace a legacy kind the asset lane rejects on a row and its chain.
    /// `dry_run` asks the server to validate and change nothing.
    pub(crate) async fn reclassify_kind(
        &self,
        asset_id: Uuid,
        kind: &str,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}",
            self.base, self.project_id
        );
        let response = self
            .client
            .patch(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "kind": kind, "dry_run": dry_run }))
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document kind failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document kind response")
    }

    /// Change a chain from one accepted kind to another.
    /// `dry_run` asks the server to validate and change nothing.
    pub(crate) async fn reclassify_accepted_kind(
        &self,
        asset_id: Uuid,
        kind: &str,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}/reclassify",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "kind": kind, "dry_run": dry_run }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document reclassify failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document reclassify response")
    }

    /// Move the row's document identity to `new_slug`, across its whole chain.
    /// `dry_run` asks the server to validate and change nothing.
    pub(crate) async fn reslug(
        &self,
        asset_id: Uuid,
        new_slug: &str,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}",
            self.base, self.project_id
        );
        let response = self
            .client
            .patch(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "slug": new_slug, "reslug": true, "dry_run": dry_run }))
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document reslug failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document reslug response")
    }

    /// Restore a missing storage object from a same-hash sibling.
    pub(crate) async fn repair_storage(
        &self,
        asset_id: Uuid,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}/storage",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "dry_run": dry_run }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document repair failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse document repair response")
    }

    /// POST one retention command (`delete`, `restore`, `hold`) for the
    /// document identity `asset_id` belongs to, and return the server's JSON.
    async fn retention_command(
        &self,
        asset_id: Uuid,
        verb: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/app/api/projects/{}/documents/{asset_id}/{verb}",
            self.base, self.project_id
        );
        let request = self.client.post(&url).bearer_auth(&self.token);
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        let response = request
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "document {verb} failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).with_context(|| format!("parse document {verb} response"))
    }

    /// Soft-delete the document `asset_id` belongs to for 180 days, recording
    /// the source commit that removed its pointer.
    pub(crate) async fn soft_delete(
        &self,
        asset_id: Uuid,
        commit_sha: &str,
    ) -> Result<serde_json::Value> {
        self.retention_command(
            asset_id,
            "delete",
            Some(serde_json::json!({ "commit_sha": commit_sha })),
        )
        .await
    }

    /// Soft-delete a mistaken document chain that has no pointer-removal commit.
    pub(crate) async fn retire(&self, asset_id: Uuid) -> Result<serde_json::Value> {
        self.retention_command(
            asset_id,
            "delete",
            Some(serde_json::json!({ "manual": true })),
        )
        .await
    }

    /// Restore a soft-deleted document with its revision history.
    pub(crate) async fn restore(&self, asset_id: Uuid) -> Result<serde_json::Value> {
        self.retention_command(asset_id, "restore", None).await
    }

    /// Place (`held`) or release a Legal Hold on the document's chain.
    pub(crate) async fn set_legal_hold(
        &self,
        asset_id: Uuid,
        held: bool,
    ) -> Result<serde_json::Value> {
        self.retention_command(asset_id, "hold", Some(serde_json::json!({ "held": held })))
            .await
    }

    /// File one inbound message's attachments into the matter
    /// (`POST /app/api/projects/{id}/mail/file`, #517). Entirely
    /// server-side: only this JSON response travels back, never the
    /// attachment bytes.
    pub(crate) async fn file_mail(
        &self,
        message_id: Uuid,
        kind: &str,
        visibility: &str,
        dry_run: bool,
    ) -> Result<MailFileResponse> {
        let url = format!(
            "{}/app/api/projects/{}/mail/file",
            self.base, self.project_id
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "message_id": message_id,
                "kind": kind,
                "visibility": visibility,
                "dry_run": dry_run,
            }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "filing message {message_id} failed: {status}: {}",
                first_line(&text)
            ));
        }
        serde_json::from_str(&text).context("parse mail-file response")
    }
}

/// One attachment as `POST /app/api/projects/{id}/mail/file` reports it,
/// filed or not.
#[derive(Debug, Deserialize)]
pub(crate) struct MailFileAttachment {
    pub(crate) filename: String,
    pub(crate) content_type: String,
    pub(crate) size_bytes: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MailFileResponse {
    pub(crate) attachments: Vec<MailFileAttachment>,
    /// One pointer per filed attachment, in message-part order. Empty on a
    /// dry run, or when the message carried no attachments.
    pub(crate) filed: Vec<store::document_pointers::DocumentPointer>,
}

/// Refuse a document over the shared maximum before it is encoded or sent.
///
/// Every upload this CLI makes routes through here, not just the one that
/// has a file to stat: [`archive_repository`] builds its zip in memory, and a
/// repository snapshot is the payload most likely to run past the limit. A
/// request carrying more than [`MAX_DOCUMENT_UPLOAD_REQUEST_BYTES`] never
/// reaches the door's own size check at all — it dies at the framework's
/// body cap and reports a raw buffering error, which is the failure ENG-602
/// set out to replace.
///
/// [`MAX_DOCUMENT_UPLOAD_REQUEST_BYTES`]: store::documents::MAX_DOCUMENT_UPLOAD_REQUEST_BYTES
fn validate_document_upload_size_in_bytes(kind: &str, actual: usize) -> Result<()> {
    if actual > store::documents::max_document_upload_bytes(kind) {
        return Err(anyhow!(
            store::documents::document_upload_size_message_for_kind(kind, actual)
        ));
    }
    Ok(())
}

fn validate_document_upload_size(kind: &str, file: &Path) -> Result<()> {
    // Reject locally so an oversized file never opens a remote upload.
    let actual = usize::try_from(
        file.metadata()
            .with_context(|| format!("read metadata for {}", file.display()))?
            .len(),
    )
    .context("document size does not fit in memory")?;
    validate_document_upload_size_in_bytes(kind, actual)
}

/// `navigator site document upload --project <code> --file … --kind …`
/// — file a local document into a matter through the REST door
/// (`POST /app/api/projects/{id}/documents`). `--kind` is required and must
/// be an asset-lane classification; the CLI does not default it.
#[allow(clippy::too_many_arguments)]
pub async fn document_upload(
    host: Option<&str>,
    project_code: &str,
    file: &Path,
    kind: &str,
    visibility: Option<&str>,
    description: Option<&str>,
    content_type: Option<&str>,
    slug: Option<&str>,
    transcript_quality: Option<&str>,
    execution: &DocumentExecution,
    folder: Option<store::document_pointers::MatterFolder>,
) -> ExitCode {
    document_upload_from(
        Path::new("."),
        host,
        project_code,
        file,
        kind,
        visibility,
        description,
        content_type,
        slug,
        transcript_quality,
        execution,
        folder,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn document_upload_from(
    root: &Path,
    host: Option<&str>,
    project_code: &str,
    file: &Path,
    kind: &str,
    visibility: Option<&str>,
    description: Option<&str>,
    content_type: Option<&str>,
    slug: Option<&str>,
    transcript_quality: Option<&str>,
    execution: &DocumentExecution,
    folder: Option<store::document_pointers::MatterFolder>,
) -> ExitCode {
    run(async {
        if transcript_quality.is_some() && kind != "transcript" {
            return Err(anyhow!("--quality is valid only with --kind transcript"));
        }
        let target = crate::upload_slug::resolve(root, project_code, file, slug)?;
        validate_document_upload_size(kind, &target.file)?;
        let bytes = std::fs::read(&target.file)
            .with_context(|| format!("read {}", target.file.display()))?;
        let client = DocumentClient::connect(host, project_code).await?;
        file_upload_target(
            &client,
            project_code,
            &target,
            &bytes,
            kind,
            visibility,
            description,
            content_type,
            transcript_quality,
            execution,
            folder,
        )
        .await
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn file_upload_target(
    client: &DocumentClient,
    project_code: &str,
    target: &crate::upload_slug::UploadTarget,
    bytes: &[u8],
    kind: &str,
    visibility: Option<&str>,
    description: Option<&str>,
    content_type: Option<&str>,
    transcript_quality: Option<&str>,
    execution: &DocumentExecution,
    folder: Option<store::document_pointers::MatterFolder>,
) -> Result<()> {
    if transcript_quality.is_some() && kind != "transcript" {
        return Err(anyhow!("--quality is valid only with --kind transcript"));
    }
    let (root, file, slug) = (
        target.root.as_path(),
        target.file.as_path(),
        target.slug.as_deref(),
    );
    if let Some(notice) = &target.notice {
        eprintln!("{notice}");
    }
    // The matter folder the bytes are kept in: `--folder`, else the one
    // `--file` already sits in (LAW-189).
    let folder = folder.or_else(|| {
        let documents =
            std::fs::canonicalize(root.join(crate::document_sync::DOCUMENT_DIRECTORY)).ok()?;
        let canonical = std::fs::canonicalize(file).ok()?;
        crate::document_layout::matter_folder_of(canonical.strip_prefix(documents).ok()?)
    });
    let mut pointer = client
        .upload_bytes_with_details(
            file.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow!("file path has no UTF-8 filename"))?,
            bytes,
            kind,
            visibility,
            description,
            content_type,
            slug,
            None,
            None,
            transcript_quality,
            execution,
        )
        .await?;
    if let Some(folder) = folder {
        pointer.folder = crate::document_layout::recorded_folder(&pointer.kind, folder);
    }
    if let Some(notice) = visibility_mismatch_notice(
        visibility,
        &pointer.visibility,
        pointer.current_version.asset_id,
    ) {
        eprintln!("{notice}");
    }
    if crate::projects::manifest::read(root)
        .and_then(|manifest| manifest.project)
        .as_deref()
        == Some(project_code)
    {
        let filename = file
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("file path has no filename"))?;
        let document_slug = slug.unwrap_or(filename);
        let pointer_path =
            crate::document_sync::write_uploaded_pointer(root, document_slug, &pointer)?;
        println!("staged {}", pointer_path.display());
    }
    print!("{}", pointer.to_yaml()?);
    Ok(())
}

/// Upload a directory using the supported browser login once if the session expires.
#[allow(clippy::too_many_arguments)]
pub async fn document_upload_directory(
    host: Option<&str>,
    project_code: &str,
    dir: &Path,
    kind: &str,
    visibility: Option<&str>,
    description: Option<&str>,
    content_type: Option<&str>,
    transcript_quality: Option<&str>,
    execution: &DocumentExecution,
    folder: Option<store::document_pointers::MatterFolder>,
) -> ExitCode {
    document_upload_directory_from(
        Path::new("."),
        host,
        project_code,
        dir,
        kind,
        visibility,
        description,
        content_type,
        transcript_quality,
        execution,
        folder,
        |base| async move {
            eprintln!("Session expired; pausing the batch for sign-in once.");
            if crate::login::run_login(&base, false).await != ExitCode::SUCCESS {
                return Err(anyhow!(
                    "sign-in did not complete; remaining files are unfiled"
                ));
            }
            Ok(())
        },
    )
    .await
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn document_upload_directory_from<F, Fut>(
    root: &Path,
    host: Option<&str>,
    project_code: &str,
    dir: &Path,
    kind: &str,
    visibility: Option<&str>,
    description: Option<&str>,
    content_type: Option<&str>,
    transcript_quality: Option<&str>,
    execution: &DocumentExecution,
    folder: Option<store::document_pointers::MatterFolder>,
    mut login: F,
) -> ExitCode
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    run(async {
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            root.join(dir)
        };
        let files = crate::upload_slug::directory_files(&dir)?;
        let mut table = Table::new();
        table.set_header(["File", "Outcome"]);
        let mut completed = std::collections::HashMap::<String, std::path::PathBuf>::new();
        let mut uncertain = std::collections::HashMap::<String, std::path::PathBuf>::new();
        let mut slugs = std::collections::HashSet::new();
        let mut login_attempted = false;
        let mut auth_stopped = false;
        let mut failed = 0usize;
        for file in files {
            let prepared = (|| -> Result<_> {
                let mut target = crate::upload_slug::resolve(root, project_code, &file, None)?;
                let filename = target
                    .file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| anyhow!("file path has no UTF-8 filename"))?;
                let slug = target.slug.get_or_insert_with(|| filename.to_string());
                // The upload door trims an explicit slug. Detect identities on that same spelling.
                if target.notice.is_some() && slug.trim() != slug {
                    return Err(anyhow!(
                        "pointer slug has surrounding whitespace; reslug before uploading"
                    ));
                }
                *slug = slug.trim().to_string();
                store::documents::validate_document_slug(filename, slug)?;
                validate_document_upload_size(kind, &target.file)?;
                let bytes = std::fs::read(&target.file)
                    .with_context(|| format!("read {}", target.file.display()))?;
                Ok((target, bytes))
            })();
            let (target, bytes) = match prepared {
                Ok(value) => value,
                Err(error) => {
                    failed += 1;
                    table.add_row([file.display().to_string(), format!("unfiled: {error:#}")]);
                    continue;
                }
            };
            let digest = store::assets::sha256_hex(&bytes);
            if let Some(original) = completed.get(&digest) {
                table.add_row([
                    file.display().to_string(),
                    format!("duplicate of {}", original.display()),
                ]);
                continue;
            }
            if let Some(original) = uncertain.get(&digest) {
                failed += 1;
                table.add_row([
                    file.display().to_string(),
                    format!(
                        "unfiled: same bytes as unsuccessful {}; reconcile before retrying",
                        original.display()
                    ),
                ]);
                continue;
            }
            if !slugs.insert(target.slug.clone()) {
                failed += 1;
                table.add_row([file.display().to_string(), "unfiled: slug collision".into()]);
                continue;
            }
            let result = loop {
                if auth_stopped {
                    break Err(anyhow!("sign-in required; batch paused"));
                }
                let result = async {
                    // Reload on every file and after login; never reuse an expired bearer.
                    let client = DocumentClient::connect(host, project_code).await?;
                    file_upload_target(
                        &client,
                        project_code,
                        &target,
                        &bytes,
                        kind,
                        visibility,
                        description,
                        content_type,
                        transcript_quality,
                        execution,
                        folder,
                    )
                    .await
                }
                .await;
                let Some(expired) = result
                    .as_ref()
                    .err()
                    .and_then(|e| e.downcast_ref::<UploadSessionExpired>())
                else {
                    break result;
                };
                if login_attempted {
                    auth_stopped = true;
                    break result;
                }
                login_attempted = true;
                if let Err(error) = Box::pin(login(expired.0.clone())).await {
                    auth_stopped = true;
                    break Err(error);
                }
            };
            match result {
                Ok(()) => {
                    completed.insert(digest, file.clone());
                    table.add_row([file.display().to_string(), "filed".into()]);
                }
                Err(error) => {
                    uncertain.insert(digest, file.clone());
                    failed += 1;
                    table.add_row([file.display().to_string(), format!("unfiled: {error:#}")]);
                }
            }
        }
        println!("{table}");
        if failed != 0 {
            return Err(anyhow!(
                "{failed} file(s) remain unfiled; successful uploads are preserved"
            ));
        }
        Ok(())
    })
    .await
}

/// `navigator site document transcribe <pointer>` — locally OCR the current
/// source revision and file its linked transcript under a stable Project slug.
pub async fn document_transcribe(pointer_path: &Path) -> ExitCode {
    document_transcribe_from(Path::new("."), pointer_path).await
}

async fn document_transcribe_from(root: &Path, pointer_path: &Path) -> ExitCode {
    run(async {
        let manifest = crate::projects::manifest::read(root)
            .ok_or_else(|| anyhow!("no navigator.yaml in the current directory"))?;
        let host = manifest
            .host
            .as_deref()
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| anyhow!("navigator.yaml names no host"))?;
        let project = manifest
            .project
            .as_deref()
            .filter(|project| !project.trim().is_empty())
            .ok_or_else(|| anyhow!("navigator.yaml names no project.name"))?;
        let pointer_path = if pointer_path.is_absolute() {
            pointer_path.to_path_buf()
        } else {
            root.join(pointer_path)
        };
        let source_slug = crate::document_read::slug_from_pointer(root, &pointer_path)?;
        let pointer_path =
            crate::document_sync::pointer_path_for_slug(root, Path::new(&source_slug));
        let pointer = crate::document_sync::read_pointer(&pointer_path)?
            .ok_or_else(|| anyhow!("{} does not exist", pointer_path.display()))?;
        if !source_slug.to_ascii_lowercase().ends_with(".pdf") {
            return Err(anyhow!("transcribe requires a PDF document pointer"));
        }
        let client = DocumentClient::connect(Some(host), project).await?;
        let revisions = client.list_revisions(&source_slug).await?;
        let source = source_revision(&pointer.current_version, &revisions.revisions)?;
        let bytes = client.download_revision(source.asset_id).await?;
        let digest = store::assets::sha256_hex(&bytes);
        if digest != source.sha256 {
            return Err(anyhow!(
                "downloaded source bytes do not match the pointer SHA-256"
            ));
        }
        let transcript = crate::document_ocr::transcribe_pdf(&bytes)?;
        let stem = Path::new(&source_slug)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| anyhow!("source document slug has no filename stem"))?;
        let filename = format!("{stem}.transcript.md");
        let transcript_slug = format!("transcripts/{filename}");
        let derived_from = serde_json::json!({
            "document_id": source.asset_id,
            "version": source.version,
            "sha256": source.sha256,
        });
        let filed = client
            .upload_bytes_with_details(
                &filename,
                transcript.as_bytes(),
                "transcript",
                Some("internal"),
                Some("Machine-generated OCR; verify quotations against the source scan."),
                Some("text/markdown; charset=utf-8"),
                Some(&transcript_slug),
                None,
                Some(derived_from),
                Some("machine"),
                &DocumentExecution::default(),
            )
            .await?;
        print!("{}", filed.to_yaml()?);
        Ok(())
    })
    .await
}

fn source_revision<'a>(
    pointer: &store::document_pointers::PointerVersion,
    revisions: &'a [RevisionSummary],
) -> Result<&'a RevisionSummary> {
    let source = revisions
        .iter()
        .find(|revision| revision.asset_id == pointer.asset_id)
        .ok_or_else(|| {
            anyhow!("source document's Pointer revision is no longer on the live document chain")
        })?;
    if !source.operative || source.version != pointer.version || source.sha256 != pointer.sha256 {
        return Err(anyhow!(
            "source document's Pointer is stale; run `navigator project gate --check` before transcribing"
        ));
    }
    Ok(source)
}

/// `navigator site document slug <asset_id> --project <code> --slug …`
/// — set a slug on a live row that has none.
pub async fn document_slug(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    slug: &str,
    kind: Option<&str>,
    dry_run: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = client.assign_slug(asset_id, slug, kind, dry_run).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        if let Some(stored) = body["kind"].as_str().filter(|stored| !is_asset_kind(stored)) {
            eprintln!(
                "warning: kind `{stored}` is not an accepted document kind, and the gate will reject \
                 this row's pointer; replace it with `navigator site document kind {asset_id} \
                 --project {project_code} --kind <kind>`"
            );
        }
        Ok(())
    })
    .await
}

fn is_asset_kind(kind: &str) -> bool {
    rules::kind::Kind::parse(kind).is_some_and(|parsed| parsed.valid_for(rules::kind::Lane::Asset))
}

/// `navigator site document kind <asset_id> --project <code> --kind …`
/// — replace a legacy kind the asset lane rejects on a live row.
pub async fn document_kind(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    kind: &str,
    dry_run: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = client.reclassify_kind(asset_id, kind, dry_run).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        Ok(())
    })
    .await
}

/// `navigator site document reclassify <asset_id> --project <code> --kind …`
/// — change a live chain from one accepted kind to another.
pub async fn document_reclassify(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    kind: &str,
    dry_run: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = client
            .reclassify_accepted_kind(asset_id, kind, dry_run)
            .await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        Ok(())
    })
    .await
}

/// An upload that matches the live revision's bytes files nothing, so the
/// row keeps the visibility it already had. Say so rather than print a pointer
/// that quietly disagrees with `--visibility` (LAW-102).
fn visibility_mismatch_notice(
    requested: Option<&str>,
    filed: &str,
    asset_id: Uuid,
) -> Option<String> {
    // Match to fixed labels before formatting; no caller-supplied string is returned or logged.
    let requested = match requested {
        Some("client") if filed != "client" => "client",
        Some("internal") if filed != "internal" => "internal",
        Some(other) if other != filed => "unrecognized",
        _ => return None,
    };
    let filed = match filed {
        "client" => "client",
        "internal" => "internal",
        _ => "unrecognized",
    };
    Some(format!(
        "warning: the document is filed as `{filed}`, not `{requested}`: identical bytes under          this slug file no new revision, so the existing visibility stays; change it with          `navigator site document visibility {asset_id} --visibility {requested}`"
    ))
}

/// The slug a file under the checkout's `documents/` folder files as when no
/// `--slug` is given: its path below `documents/`, so
/// `documents/onboarding/letter.pdf` is `onboarding/letter.pdf` and its
/// pointer lands at the mirrored `blobs/onboarding/letter.pdf.yaml` (LAW-102). `None` for a file at the
/// `documents/` root or outside it, which keep the filename as the slug.
pub(crate) fn slug_from_documents_folder(root: &Path, file: &Path) -> Option<String> {
    // Canonical first, so the on-disk case and any `..` agree; the typed path
    // is the fallback for a file that does not exist yet.
    let relative = (|| {
        let root = std::fs::canonicalize(root).ok()?;
        let file = std::fs::canonicalize(file).ok()?;
        file.strip_prefix(root).ok().map(Path::to_path_buf)
    })()
    .or_else(|| file.strip_prefix(root).ok().map(Path::to_path_buf));
    let relative = relative.unwrap_or_else(|| file.to_path_buf());
    let parts: Vec<&str> = relative
        .components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    // A matter folder — including a fixed `Work Product` subfolder, whose
    // name spans two components (LAW-215) — is where the bytes live, not
    // part of the slug (LAW-189).
    if let Some(prefix) = documents_folder_prefix_len(&parts) {
        return Some(parts[1 + prefix..].join("/"));
    }
    if parts.len() > 2 && parts[0] == "documents" {
        return Some(parts[1..].join("/"));
    }
    None
}

/// How many leading components of `parts` spell a matter folder's name —
/// two for a fixed `Work Product` subfolder (LAW-215), one for every other.
/// The longest match wins, so `Work Product/Research (Work Product)/` never
/// degrades to its bare parent.
fn documents_folder_prefix_len(parts: &[&str]) -> Option<usize> {
    // Folder names are spelled below `documents/`; the leading component is
    // the checkout's documents root itself.
    if parts.first() != Some(&"documents") {
        return None;
    }
    let segments = &parts[1..];
    store::document_pointers::MatterFolder::ALL
        .iter()
        .filter_map(|folder| {
            let names: Vec<&str> = folder.as_str().split('/').collect();
            (segments.len() > names.len() && segments[..names.len()] == names[..])
                .then_some(names.len())
        })
        .max()
}

/// `navigator site document visibility <asset_id> --project <code> --visibility …`
/// — change who can see a filed revision, without filing a new one.
pub async fn document_visibility(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    visibility: &str,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        client.set_visibility(asset_id, visibility).await?;
        println!("{asset_id} is now {visibility}");
        Ok(())
    })
    .await
}

/// `navigator site document reslug <asset_id> --project <code> --slug …`
/// — move a document identity to another valid slug across the row's whole
/// chain, then move the checkout's committed pointer to follow it.
pub async fn document_reslug(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    slug: &str,
    dir: &Path,
    dry_run: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = reslug_with_local(&client, asset_id, slug, dir, dry_run).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        Ok(())
    })
    .await
}

/// `navigator site document retire <asset_id> --project <code> --yes-retire`
/// — soft-delete a mistaken chain that has no pointer-removal commit.
pub async fn document_retire(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    dry_run: bool,
    yes_retire: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = retire_document(&client, asset_id, dry_run, yes_retire).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        if !dry_run {
            if let Some(slug) = body["slug"].as_str() {
                eprintln!(
                    "remove the local pointer blobs/{slug}.yaml and byte copy documents/{slug} before the next project sync"
                );
            }
        }
        Ok(())
    })
    .await
}

async fn retire_document(
    client: &DocumentClient,
    asset_id: Uuid,
    dry_run: bool,
    yes_retire: bool,
) -> Result<serde_json::Value> {
    let documents = client.list_documents().await?;
    let selected = documents
        .iter()
        .find(|document| document.id == asset_id)
        .ok_or_else(|| anyhow!("asset {asset_id} is not a live document on this matter"))?;
    let slug = selected
        .slug
        .as_deref()
        .ok_or_else(|| anyhow!("asset {asset_id} has no Document Identity to retire"))?;
    if selected.kind.as_deref() == Some("closed_repository") {
        return Err(anyhow!("closed_repository archives cannot be retired"));
    }
    let revisions = documents
        .iter()
        .filter(|document| document.slug.as_deref() == Some(slug))
        .count();
    if dry_run {
        return Ok(serde_json::json!({
            "slug": slug,
            "revisions": revisions,
            "dry_run": true,
        }));
    }
    if !yes_retire {
        return Err(anyhow!(
            "retiring {slug} needs an explicit confirmation; pass --yes-retire"
        ));
    }
    client.retire(asset_id).await
}

/// Validate the live move first, preflight both local destinations, then
/// change the live identity and move its local pair. A rejected server write
/// leaves the checkout untouched; a local failure attempts to restore the
/// live identity to its prior slug.
pub(crate) async fn reslug_with_local(
    client: &DocumentClient,
    asset_id: Uuid,
    new_slug: &str,
    dir: &Path,
    dry_run: bool,
) -> Result<serde_json::Value> {
    let preview = client.reslug(asset_id, new_slug, true).await?;
    let previous = preview["previous_slug"]
        .as_str()
        .ok_or_else(|| anyhow!("document reslug preview omitted previous_slug"))?;
    let target = preview["slug"]
        .as_str()
        .ok_or_else(|| anyhow!("document reslug preview omitted slug"))?;
    validate_local_pointer_move(dir, previous, target)?;
    if dry_run {
        return Ok(preview);
    }

    let moved_live = match client.reslug(asset_id, target, false).await {
        Ok(response) => response,
        Err(error) => {
            let reslug_error = format!("{error:#}");
            let inventory = client.list_documents().await.map_err(|inventory_error| {
                anyhow!(
                    "{reslug_error}; the live reslug response was interrupted and its result could not be checked: {inventory_error:#}; compare the live slug with the checkout before retrying"
                )
            })?;
            match inventory.iter().find(|document| document.id == asset_id) {
                Some(document) if document.slug.as_deref() == Some(target) => {
                    serde_json::json!({
                        "slug": target,
                        "previous_slug": previous,
                        "dry_run": false,
                        "recovered_after_interruption": true,
                    })
                }
                Some(document) if document.slug.as_deref() == Some(previous) => {
                    return Err(anyhow!(reslug_error));
                }
                _ => {
                    return Err(anyhow!(
                        "{reslug_error}; the live reslug response was interrupted and the live identity is neither at the old nor target slug; reconcile the checkout manually"
                    ));
                }
            }
        }
    };
    let actual_previous = moved_live["previous_slug"].as_str();
    let actual_target = moved_live["slug"].as_str();
    if actual_previous != Some(previous) || actual_target != Some(target) {
        return Err(anyhow!(
            "document reslug changed concurrently; the checkout was left untouched, run `navigator project gate --check`"
        ));
    }

    let moved = match move_local_pointer(dir, previous, target) {
        Ok(moved) => moved,
        Err(error) => {
            return match client.reslug(asset_id, previous, false).await {
                Ok(_) => Err(error.context("local move failed; the live identity was restored")),
                Err(rollback) => Err(error.context(format!(
                    "local move failed and the live identity could not be restored: {rollback:#}"
                ))),
            };
        }
    };
    if let Some(moved) = moved {
        println!("moved {}", moved.pointer);
        if let Some(bytes) = moved.bytes {
            println!("moved {bytes}");
        }
    }
    Ok(moved_live)
}

/// What [`move_local_pointer`] renamed under the checkout.
#[derive(Debug, PartialEq, Eq)]
struct CheckoutMove {
    pointer: String,
    bytes: Option<String>,
}

/// The local key under `dir` for `slug` — its Blob is `blobs/<key>.yaml` and
/// its bytes `documents/<key>` — which a legacy slug with no extension of its
/// own keeps as `<slug>.<ext>`, using the extension of the slug it moved to.
fn pointer_candidates(slug: &str, extension_from: &str) -> Vec<String> {
    let mut candidates = vec![slug.to_string()];
    if let Some(extension) = Path::new(extension_from)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
    {
        if !slug.ends_with(&format!(".{extension}")) {
            candidates.push(format!("{slug}.{extension}"));
        }
    }
    candidates
}

/// The root-relative Blob for a local key.
fn blob_relative(key: &str) -> String {
    format!("{}/{key}.yaml", crate::document_sync::POINTER_DIRECTORY)
}

/// The root-relative bytes for a local key.
fn bytes_relative(key: &str) -> String {
    format!("{}/{key}", crate::document_sync::DOCUMENT_DIRECTORY)
}

/// The root-relative bytes for a local key described by the pointer at
/// `pointer`: inside the matter folder it names (LAW-189), or at the key
/// itself when the pointer cannot be read.
fn laid_out_bytes_relative(dir: &Path, key: &str, pointer: &str) -> String {
    match crate::document_sync::read_pointer(&dir.join(pointer)) {
        Ok(Some(pointer)) => {
            let relative = if let Some(path) = pointer.checkout_path.as_deref() {
                PathBuf::from(path)
            } else {
                crate::document_layout::bytes_relative(Path::new(key), pointer.matter_folder())
            };
            format!(
                "{}/{}",
                crate::document_sync::DOCUMENT_DIRECTORY,
                relative.to_string_lossy().replace('\\', "/")
            )
        }
        _ => bytes_relative(key),
    }
}

/// Refuse local destinations before the live Document Identity changes.
fn validate_local_pointer_move(dir: &Path, previous: &str, new_slug: &str) -> Result<()> {
    let Some(source) = pointer_candidates(previous, new_slug)
        .into_iter()
        .find(|key| dir.join(blob_relative(key)).is_file())
    else {
        return Ok(());
    };
    let target_bytes = laid_out_bytes_relative(dir, new_slug, &blob_relative(&source));
    let source_bytes = laid_out_bytes_relative(dir, &source, &blob_relative(&source));
    for relative in [
        Some(blob_relative(new_slug)),
        (target_bytes != source_bytes).then_some(target_bytes),
    ]
    .into_iter()
    .flatten()
    {
        if dir.join(&relative).exists() {
            return Err(anyhow!(
                "{relative} already exists in the checkout; no live or local move was made"
            ));
        }
    }
    Ok(())
}

/// Move the committed pointer for `previous` to `blobs/<new_slug>.yaml`
/// under `dir`, and its bytes under `documents/` when they were pulled down.
/// Returns `None` when the checkout holds no pointer for `previous`. Refuses
/// to overwrite an existing pointer or existing bytes, and rolls a bytes
/// rename back if the pointer cannot follow it.
fn move_local_pointer(dir: &Path, previous: &str, new_slug: &str) -> Result<Option<CheckoutMove>> {
    let Some(source_key) = pointer_candidates(previous, new_slug)
        .into_iter()
        .find(|key| dir.join(blob_relative(key)).is_file())
    else {
        return Ok(None);
    };
    let source = blob_relative(&source_key);
    let target_relative = blob_relative(new_slug);
    let target = dir.join(&target_relative);
    if target.exists() {
        return Err(anyhow!(
            "the document moved, but {target_relative} already exists in the checkout; \
             leave {source} in place and run `navigator project gate --check`"
        ));
    }
    // The bytes sit in their matter folder, or at the key itself before
    // `project sync` moved them there; they move into the folder.
    let laid_out = laid_out_bytes_relative(dir, &source_key, &source);
    let source_bytes_relative = Some(if dir.join(&laid_out).is_file() {
        laid_out
    } else {
        bytes_relative(&source_key)
    });
    let target_bytes_relative = laid_out_bytes_relative(dir, new_slug, &source);
    let target_bytes = dir.join(&target_bytes_relative);
    let same_placement = source_bytes_relative.as_deref() == Some(target_bytes_relative.as_str());
    if target_bytes.exists() && !same_placement {
        return Err(anyhow!(
            "the document moved, but {target_bytes_relative} already exists in the checkout; \
             leave {source} in place and run `navigator project gate --check`"
        ));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let moved_bytes = match source_bytes_relative {
        Some(ref source_bytes_relative) => {
            let source_bytes = dir.join(source_bytes_relative);
            if source_bytes.is_file() && !same_placement {
                if let Some(parent) = target_bytes.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create {}", parent.display()))?;
                }
                std::fs::rename(&source_bytes, &target_bytes).with_context(|| {
                    format!("move {source_bytes_relative} to {target_bytes_relative}")
                })?;
                Some(source_bytes)
            } else {
                None
            }
        }
        None => None,
    };
    if let Err(error) = std::fs::rename(dir.join(&source), &target)
        .with_context(|| format!("move {source} to {target_relative}"))
    {
        if let Some(source_bytes) = moved_bytes {
            std::fs::rename(&target_bytes, &source_bytes).with_context(|| {
                format!(
                    "restore {} after the pointer could not move: {error:#}",
                    source_bytes.display()
                )
            })?;
        }
        return Err(error);
    }
    Ok(Some(CheckoutMove {
        pointer: target_relative,
        bytes: moved_bytes.map(|_| target_bytes_relative),
    }))
}

/// `navigator site document repair <asset_id> --project <code>`
/// — restore a missing storage object from a same-hash sibling. Admin only.
pub async fn document_repair(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    dry_run: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = client.repair_storage(asset_id, dry_run).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        Ok(())
    })
    .await
}

/// `navigator site document restore <asset_id> --project <code>` — bring a
/// soft-deleted document back with its revision history. The next
/// `navigator project sync` writes its pointer again.
pub async fn document_restore(host: Option<&str>, project_code: &str, asset_id: Uuid) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = client.restore(asset_id).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        if body["changed"].as_bool() == Some(true) {
            println!(
                "{}",
                palette::dim("run `navigator project sync` to write the pointer again")
            );
        }
        Ok(())
    })
    .await
}

/// `navigator site document hold <asset_id> --project <code> [--release]` —
/// place or release a Legal Hold, which keeps a soft-deleted document out of
/// the retention purge.
pub async fn document_hold(
    host: Option<&str>,
    project_code: &str,
    asset_id: Uuid,
    held: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let body = client.set_legal_hold(asset_id, held).await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        Ok(())
    })
    .await
}

/// The second half of `navigator project close <code> [--dir .]` — zip the
/// repository's working tree at HEAD (no git history — a snapshot document,
/// not a clone), and file it as a `closed_repository` document, recording
/// the final commit SHA in the asset's `metadata` (ENG-481). The content
/// hash needs no separate recording: the server derives `sha256_hex` from
/// the uploaded bytes themselves, the same way template import already
/// records both for a notation body.
///
/// Runs after [`matter_close`] succeeds; it does not gate the close. `dir` is
/// the local checkout to archive — the repository this Project's
/// `repository_url` names.
pub async fn archive_repository(host: Option<&str>, project_code: &str, dir: &Path) -> ExitCode {
    run(async {
        let commit_sha = git_head_commit_sha(dir)?;
        let zip_bytes = git_archive_zip(dir)?;
        let client = DocumentClient::connect(host, project_code).await?;
        let kind = rules::kind::Kind::ClosedRepository.as_str();
        let slug = format!("{kind}.zip");
        let pointer = client
            .upload_bytes(
                &format!("{project_code}-closed-repository.zip"),
                &zip_bytes,
                kind,
                None,
                Some(&format!(
                    "Repository archive for {project_code} at commit {commit_sha}"
                )),
                Some("application/zip"),
                Some(&slug),
                Some(serde_json::json!({ "commit_sha": commit_sha })),
            )
            .await?;
        println!(
            "{} {}",
            palette::dim("archived repository for"),
            palette::highlight(project_code)
        );
        println!("{}  {commit_sha}", palette::dim("commit:"));
        println!(
            "{}  {}",
            palette::dim("content sha256:"),
            pointer.current_version.sha256
        );
        println!(
            "{}  {}",
            palette::dim("asset id:"),
            pointer.current_version.asset_id
        );
        Ok(())
    })
    .await
}

/// `navigator site mail file --message <id> --project <code> --kind …`
/// — file one inbound message's attachments into a matter, entirely
/// server-side (`POST /app/api/projects/{id}/mail/file`, #517). The
/// attachment bytes never reach this process; only the Pointer this
/// prints and stages under `documents/mail/<message>/` does, so the result
/// is committable without the bytes ever having transited the checkout.
/// `--dry-run` writes nothing and only lists what would be filed.
#[allow(clippy::too_many_arguments)]
pub async fn mail_file(
    root: &Path,
    host: Option<&str>,
    project_code: &str,
    message: Uuid,
    kind: &str,
    visibility: &str,
    dry_run: bool,
) -> ExitCode {
    run(async {
        let client = DocumentClient::connect(host, project_code).await?;
        let response = client.file_mail(message, kind, visibility, dry_run).await?;

        if response.attachments.is_empty() {
            println!("message {message} has no attachments — nothing to file");
            return Ok(());
        }

        if dry_run {
            for attachment in &response.attachments {
                println!(
                    "would file {} ({}, {} bytes)",
                    attachment.filename, attachment.content_type, attachment.size_bytes
                );
            }
            println!(
                "{} attachment(s) would be filed",
                response.attachments.len()
            );
            return Ok(());
        }

        let blobs_dir = root
            .join(crate::document_sync::POINTER_DIRECTORY)
            .join("mail")
            .join(message.to_string());
        std::fs::create_dir_all(&blobs_dir)
            .with_context(|| format!("create {}", blobs_dir.display()))?;
        for (index, pointer) in response.filed.iter().enumerate() {
            let path = blobs_dir.join(format!("{index}.yml"));
            crate::document_sync::write_pointer_atomically(&path, &pointer.to_yaml()?)?;
            println!("staged {}", path.display());
        }
        println!("{} attachment(s) filed", response.filed.len());
        Ok(())
    })
    .await
}

/// `git rev-parse HEAD` in `dir` — the commit the archive is a snapshot of.
fn git_head_commit_sha(dir: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("run git rev-parse HEAD in {}", dir.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git rev-parse HEAD failed in {}: {}",
            dir.display(),
            first_line(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(anyhow!(
            "git rev-parse HEAD in {} returned no commit — is this a git repository with a \
             committed HEAD?",
            dir.display()
        ));
    }
    Ok(sha)
}

/// `git archive --format=zip HEAD` in `dir` — the working tree at HEAD, with
/// no `.git` history, exactly as `git archive` always produces: a snapshot,
/// never a clone.
fn git_archive_zip(dir: &Path) -> Result<Vec<u8>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["archive", "--format=zip", "HEAD"])
        .output()
        .with_context(|| format!("run git archive --format=zip HEAD in {}", dir.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git archive failed in {}: {}",
            dir.display(),
            first_line(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    Ok(output.stdout)
}

/// An HTTP client that does **not** follow redirects, so a handler's `303`
/// (the notation lifecycle POSTs redirect on success) reads as success
/// rather than following the `Location` into an HTML page.
fn no_redirect_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build http client")
}

/// One Project's result from an `/app/api/integrations/*` door — mirrors
/// `portal::integrations_api::ProjectOutcome`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct IntegrationOutcome {
    pub(crate) project_code: Option<String>,
    pub(crate) outcome: String,
    #[serde(default)]
    pub(crate) detail: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct IntegrationReportBody {
    pub(crate) results: Vec<IntegrationOutcome>,
}

/// Post one of the authenticated Firm-integration doors and parse its
/// per-Project report. Setup composes these existing doors; it does not carry
/// provider credentials or bypass the server's AdminSession/Firm checks.
pub(crate) async fn post_integration_door(
    base: &str,
    token: &str,
    path: &str,
    payload: serde_json::Value,
) -> Result<IntegrationReportBody> {
    let url = format!("{base}{path}");
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "integration door failed: {status}: {}",
            first_line(&body)
        ));
    }
    serde_json::from_str(&body).with_context(|| format!("parse POST {url}"))
}

/// `navigator project close <project-code>` — move a matter directly to
/// `closed` through the REST lifecycle door
/// (`POST /app/api/projects/{id}/lifecycle`). Resolves the human-facing code
/// to the matter id the same way `document upload` and `projects surfaces
/// reconcile` do, through the visible-projects list, then posts the
/// transition and optional effective time. The server derives `closed_at`
/// from those command inputs; without an effective time, closing an
/// already-`closed` matter reports it unchanged, while an `archived` matter
/// refuses with a caller-readable error.
pub async fn matter_close(
    host: Option<&str>,
    project_code: &str,
    reason: store::projects::ClosureReason,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> ExitCode {
    run(async {
        let (base, token) = resolve(host)?;
        let client = reqwest::Client::new();
        let list_body = fetch_project_list(&client, &base, &token).await?;
        let projects: Vec<VisibleProject> =
            serde_json::from_str(&list_body).context("parse GET /app/api/projects")?;
        let project = projects
            .iter()
            .find(|project| project.code == project_code)
            .ok_or_else(|| anyhow!("no visible matter with code `{project_code}`"))?;
        let url = format!("{base}/app/api/projects/{}/lifecycle", project.id);
        let mut payload = serde_json::json!({ "transition": "close", "reason": reason });
        if let Some(effective_at) = effective_at {
            payload["effective_at"] = serde_json::json!(effective_at);
        }
        let response = client
            .post(&url)
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "close project `{project_code}` failed: {status}: {}",
                first_line(&body)
            ));
        }
        println!(
            "{} {}",
            palette::dim("closed matter"),
            palette::highlight(project_code)
        );
        Ok(())
    })
    .await
}

#[derive(Debug, Deserialize)]
struct VisiblePerson {
    id: Uuid,
    email: String,
    name: String,
    role: String,
}

#[derive(Debug, Deserialize)]
struct VisibleEntity {
    id: Uuid,
    name: String,
}

#[derive(Debug, Deserialize)]
struct VisibleEntityType {
    id: Uuid,
    name: String,
}

#[derive(Debug, Deserialize)]
struct VisibleJurisdiction {
    id: Uuid,
    name: String,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreatedEntityId {
    id: Uuid,
}

#[derive(Debug, Deserialize)]
struct CreatedProjectResponse {
    id: Uuid,
    name: String,
    code: String,
    status: String,
    entity_id: Uuid,
}

/// Which entity `projects_create` resolves the matter against, decided once
/// from `--entity-name`/`--jurisdiction` before any network call.
#[derive(Clone, Copy)]
enum EntitySelection<'a> {
    /// Resolve an existing entity by name.
    Existing(&'a str),
    /// Create a `Human` entity for the client, in this jurisdiction.
    HumanIn(&'a str),
}

/// Resolve an **existing** Entity by name over `GET /app/api/entities`.
/// Never creates one — a missing name is the caller's to fix.
async fn resolve_existing_entity(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    needle: &str,
) -> Result<Uuid> {
    let entities_resp = client
        .get(format!("{base}/app/api/entities"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/entities")?;
    let entities_status = entities_resp.status();
    let entities_body = entities_resp.text().await.unwrap_or_default();
    if !entities_status.is_success() {
        return Err(anyhow!(
            "list entities failed: {entities_status}: {}",
            first_line(&entities_body)
        ));
    }
    let entities: Vec<VisibleEntity> =
        serde_json::from_str(&entities_body).context("parse GET /app/api/entities")?;
    entities
        .into_iter()
        .find(|e| e.name.eq_ignore_ascii_case(needle))
        .map(|e| e.id)
        .ok_or_else(|| anyhow!("no entity named `{needle}` — create it first"))
}

/// Create a `Human` entity named for the client, in `jurisdiction_name` —
/// the Human-entity rule
/// (`docs/project-repositories.md#an-individual-clients-entity`): an
/// individual client's matter opens against a `Human` entity in the
/// client's own jurisdiction, never guessed and never the firm's own.
async fn create_human_entity(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    client_name: &str,
    jurisdiction_name: &str,
) -> Result<(Uuid, Uuid)> {
    let types_resp = client
        .get(format!("{base}/app/api/entity-types"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/entity-types")?;
    let types_status = types_resp.status();
    let types_body = types_resp.text().await.unwrap_or_default();
    if !types_status.is_success() {
        return Err(anyhow!(
            "list entity types failed: {types_status}: {}",
            first_line(&types_body)
        ));
    }
    let entity_types: Vec<VisibleEntityType> =
        serde_json::from_str(&types_body).context("parse GET /app/api/entity-types")?;
    let entity_type_id = entity_types
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case("Human"))
        .map(|t| t.id)
        .ok_or_else(|| anyhow!("no `Human` entity type on this deployment"))?;

    let jurisdictions_resp = client
        .get(format!("{base}/app/api/jurisdictions"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/jurisdictions")?;
    let jurisdictions_status = jurisdictions_resp.status();
    let jurisdictions_body = jurisdictions_resp.text().await.unwrap_or_default();
    if !jurisdictions_status.is_success() {
        return Err(anyhow!(
            "list jurisdictions failed: {jurisdictions_status}: {}",
            first_line(&jurisdictions_body)
        ));
    }
    let jurisdictions: Vec<VisibleJurisdiction> =
        serde_json::from_str(&jurisdictions_body).context("parse GET /app/api/jurisdictions")?;
    let jurisdiction_id = jurisdictions
        .into_iter()
        .find(|j| {
            j.name.eq_ignore_ascii_case(jurisdiction_name)
                || j.code
                    .as_deref()
                    .is_some_and(|code| code.eq_ignore_ascii_case(jurisdiction_name))
        })
        .map(|j| j.id)
        .ok_or_else(|| anyhow!("no jurisdiction named `{jurisdiction_name}`"))?;

    let create_resp = client
        .post(format!("{base}/app/api/entities"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": client_name,
            "entity_type_id": entity_type_id,
            "jurisdiction_id": jurisdiction_id,
        }))
        .send()
        .await
        .context("POST /app/api/entities")?;
    let create_status = create_resp.status();
    let create_body = create_resp.text().await.unwrap_or_default();
    if !create_status.is_success() {
        return Err(anyhow!(
            "create the Human entity for `{client_name}` failed: {create_status}: {}",
            first_line(&create_body)
        ));
    }
    let created: CreatedEntityId =
        serde_json::from_str(&create_body).context("parse POST /app/api/entities")?;
    Ok((created.id, jurisdiction_id))
}

/// Validate a Human Entity's type and jurisdiction without writing anything.
/// The scaffold runs this before creating a missing contact, so an invalid
/// jurisdiction cannot leave a partially prepared directory behind.
async fn validate_human_entity_selection(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    jurisdiction_name: &str,
) -> Result<()> {
    let entity_types = client
        .get(format!("{base}/app/api/entity-types"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/entity-types")?;
    let status = entity_types.status();
    let body = entity_types.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "list entity types failed: {status}: {}",
            first_line(&body)
        ));
    }
    let entity_types: Vec<VisibleEntityType> =
        serde_json::from_str(&body).context("parse GET /app/api/entity-types")?;
    if !entity_types
        .iter()
        .any(|entity_type| entity_type.name.eq_ignore_ascii_case("Human"))
    {
        return Err(anyhow!("no `Human` entity type on this deployment"));
    }

    let jurisdictions = client
        .get(format!("{base}/app/api/jurisdictions"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/jurisdictions")?;
    let status = jurisdictions.status();
    let body = jurisdictions.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "list jurisdictions failed: {status}: {}",
            first_line(&body)
        ));
    }
    let jurisdictions: Vec<VisibleJurisdiction> =
        serde_json::from_str(&body).context("parse GET /app/api/jurisdictions")?;
    if !jurisdictions.iter().any(|jurisdiction| {
        jurisdiction.name.eq_ignore_ascii_case(jurisdiction_name)
            || jurisdiction
                .code
                .as_deref()
                .is_some_and(|code| code.eq_ignore_ascii_case(jurisdiction_name))
    }) {
        return Err(anyhow!("no jurisdiction named `{jurisdiction_name}`"));
    }
    Ok(())
}

async fn resolve_entity_selection(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    client_name: &str,
    selection: EntitySelection<'_>,
) -> Result<(Uuid, Option<Uuid>)> {
    match selection {
        EntitySelection::Existing(needle) => Ok((
            resolve_existing_entity(client, base, token, needle).await?,
            None,
        )),
        EntitySelection::HumanIn(jurisdiction_name) => {
            let (entity_id, jurisdiction_id) =
                create_human_entity(client, base, token, client_name, jurisdiction_name).await?;
            Ok((entity_id, Some(jurisdiction_id)))
        }
    }
}

/// `navigator project create --code --name --client-email [--entity-name |
/// --jurisdiction] [--closed --closed-at] --attest` — open a matter through
/// the live site's `POST /app/api/projects`, the caller's own bearer token
/// attached so the conflict attestation stays a personal act rather than a
/// service credential's. The only CLI door onto `store::projects::open_matter`
/// now runs over HTTP; there is no local-store equivalent that connects to
/// `NAVIGATOR_SURREAL_*` directly, so this works against any deployment the
/// operator has a login for, with no database connection required.
///
/// Resolves `--client-email` to the client-of-record Person over
/// `GET /app/api/people` (the deployment carries no filtered lookup, so this
/// reads the whole visible directory and matches case-insensitively, the
/// same shape `projects_close` already uses for a matter code). With
/// `--entity-name`, resolves an **existing** Entity by that name over
/// `GET /app/api/entities` — this door never creates one. Without it,
/// creates a `Human` entity named for the client, in `--jurisdiction`
/// (required in that case): the Human-entity rule
/// (`docs/project-repositories.md#an-individual-clients-entity`) says an
/// individual client's matter opens against a `Human` entity in the
/// client's own jurisdiction, never guessed and never the firm's own.
///
/// `--attest` is the operator's explicit affirmation that the attorney has
/// checked for conflicts, and that either none prevent the open or this
/// Project is not legal advice; the server refuses the open without it.
/// `--closed`/`--closed-at` open the matter already closed (ENG-469) — both
/// required together, refused alone.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub async fn projects_create(
    host: Option<&str>,
    name: &str,
    code: &str,
    client_email: &str,
    entity_name: Option<&str>,
    jurisdiction: Option<&str>,
    attest: bool,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> ExitCode {
    projects_create_named(
        host,
        name,
        code,
        "",
        client_email,
        entity_name,
        jurisdiction,
        attest,
        closed_at,
        None,
        None,
    )
    .await
}

/// Open with an explicit authorized Firm and brand, validated before creating contacts.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn projects_create_named(
    host: Option<&str>,
    name: &str,
    code: &str,
    client_name: &str,
    client_email: &str,
    entity_name: Option<&str>,
    jurisdiction: Option<&str>,
    attest: bool,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    firm: Option<&str>,
    brand: Option<&str>,
) -> ExitCode {
    run(async {
        // Caller-correctable and independent of any connection: resolved
        // before requiring a login, matching every other "is this
        // combination even sensible" refusal in this command. Resolving it
        // once here, rather than matching on the raw `Option`s again where
        // `entity_id` is computed, is what keeps that later site provably
        // exhaustive with no unreachable-in-practice branch to justify.
        let entity_selection = match (entity_name, jurisdiction) {
            (Some(needle), _) => EntitySelection::Existing(needle),
            (None, Some(jurisdiction_name)) => EntitySelection::HumanIn(jurisdiction_name),
            (None, None) => {
                return Err(anyhow!(
                    "an individual client's matter opens against a Human entity in the \
                     client's own jurisdiction — pass --jurisdiction <name> (e.g. \
                     --jurisdiction Nevada), or pass --entity-name to open against an \
                     existing entity instead"
                ));
            }
        };

        let (base, token) = resolve(host)?;
        let client = reqwest::Client::new();

        let ownership = match (firm, brand) {
            (Some(firm), Some(brand)) => Some((resolve_firm(&base, &token, firm, Some(brand)).await?, brand)),
            (None, None) => None,
            _ => anyhow::bail!("--firm and --brand must be supplied together"),
        };

        let people_resp = client
            .get(format!("{base}/app/api/people"))
            .bearer_auth(&token)
            .send()
            .await
            .context("GET /app/api/people")?;
        let people_status = people_resp.status();
        let people_body = people_resp.text().await.unwrap_or_default();
        if !people_status.is_success() {
            return Err(anyhow!(
                "list people failed: {people_status}: {}",
                first_line(&people_body)
            ));
        }
        let people: Vec<VisiblePerson> =
            serde_json::from_str(&people_body).context("parse GET /app/api/people")?;
        // Resolve caller-supplied identity coordinates before any create:
        // duplicate entities and unknown jurisdictions must not leave a new
        // contact without a Project.
        match entity_selection {
            EntitySelection::Existing(needle) => {
                resolve_existing_entity(&client, &base, &token, needle).await?;
            }
            EntitySelection::HumanIn(jurisdiction_name) => {
                validate_human_entity_selection(&client, &base, &token, jurisdiction_name).await?;
            }
        }
        let person = match people
            .into_iter()
            .find(|p| p.email.eq_ignore_ascii_case(client_email))
        {
            Some(person) => person,
            None if !client_name.trim().is_empty() => {
                let response = client
                    .post(format!("{base}/app/api/people"))
                    .bearer_auth(&token)
                    .json(&serde_json::json!({"name":client_name,"email":client_email,"role":"client"}))
                    .send()
                    .await
                    .context("POST /app/api/people")?;
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    return Err(anyhow!("create client contact failed: {status}: {}", first_line(&body)));
                }
                serde_json::from_str::<VisiblePerson>(&body).context("parse POST /app/api/people")?
            }
            None => return Err(anyhow!("no person with email `{client_email}` — create it first with `navigator site import person <seed-file>`")),
        };
        if !person.role.eq_ignore_ascii_case("client") {
            return Err(anyhow!(
                "the client of record `{client_email}` must be a client person, not {}",
                person.role
            ));
        }

        let (entity_id, jurisdiction_id) =
            resolve_entity_selection(&client, &base, &token, &person.name, entity_selection)
                .await?;

        let mut payload = serde_json::json!({
            "name": name,
            "code": code,
            "client_id": person.id,
            "entity_id": entity_id,
            "attestation": attest,
        });
        if let Some((firm_id, brand)) = ownership {
            payload["firm_id"] = serde_json::json!(firm_id);
            payload["brand"] = serde_json::json!(brand);
        }
        if let Some(jurisdiction_id) = jurisdiction_id {
            payload["jurisdiction_id"] = serde_json::json!(jurisdiction_id);
        }
        if let Some(closed_at) = closed_at {
            payload["status"] = serde_json::json!("closed");
            payload["closed_at"] = serde_json::json!(closed_at);
        }
        let response = client
            .post(format!("{base}/app/api/projects"))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .context("POST /app/api/projects")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "create project `{code}` failed: {status}: {}",
                first_line(&body)
            ));
        }
        let created: CreatedProjectResponse =
            serde_json::from_str(&body).context("parse POST /app/api/projects")?;
        println!(
            "{} {} (code={}, status={}, entity_id={})",
            palette::dim(format!("created project {}", created.id)),
            palette::highlight(&created.name),
            created.code,
            created.status,
            created.entity_id,
        );
        println!(
            "{}",
            palette::dim(format!(
                "open a notation with: navigator site notation create <template_code> --project {}",
                created.code
            )),
        );
        Ok(())
    })
    .await
}

/// Check the visible Project directory for an immutable code before the
/// scaffold creates contacts, Entities, or a Project. The server still owns
/// uniqueness and closes a race between this read and the final open.
pub async fn project_code_exists(host: Option<&str>, code: &str) -> Result<bool> {
    let (base, token) = resolve(host)?;
    let response = reqwest::Client::new()
        .get(format!("{base}/app/api/projects"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/projects")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "list Projects failed: {status}: {}",
            first_line(&body)
        ));
    }
    let projects: Vec<serde_json::Value> =
        serde_json::from_str(&body).context("parse GET /app/api/projects")?;
    Ok(projects
        .iter()
        .any(|project| project.get("code").and_then(serde_json::Value::as_str) == Some(code)))
}

/// Resolve an operator-entered Firm name from the caller's authorized
/// directory and update a live Project's Firm/brand pair.
pub async fn set_project_firm_brand(
    host: &str,
    project_code: &str,
    firm_name: &str,
    brand: &str,
) -> anyhow::Result<()> {
    let (base, token) = resolve(Some(host))?;
    let project_id =
        crate::projects::surfaces::resolve_project_id(&base, &token, project_code).await?;
    let firm_id = resolve_firm(&base, &token, firm_name, Some(brand)).await?;
    let response = reqwest::Client::new()
        .patch(format!("{base}/app/api/projects/{project_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "firm_id": firm_id, "brand": brand }))
        .send()
        .await
        .context("PATCH /app/api/projects/{id}")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "set Project Firm and brand failed: {status}: {}",
            body.lines().next().unwrap_or_default().trim()
        ));
    }
    Ok(())
}

/// Resolve a Firm name only within the caller's authorized directory.
pub(crate) async fn resolve_firm(
    base: &str,
    token: &str,
    needle: &str,
    brand: Option<&str>,
) -> Result<uuid::Uuid> {
    #[derive(serde::Deserialize)]
    struct Firm {
        id: uuid::Uuid,
        name: String,
        #[serde(default)]
        brands: Vec<String>,
    }
    let response = reqwest::Client::new()
        .get(format!("{base}/app/api/firms"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/firms")?;
    let response = response
        .error_for_status()
        .context("list authorized Firms")?;
    let firms: Vec<Firm> = response.json().await.context("parse Firm directory")?;
    let matches: Vec<_> = firms
        .iter()
        .filter(|firm| firm.name.eq_ignore_ascii_case(needle.trim()))
        .collect();
    let firm = match matches.as_slice() {
        [firm] => firm,
        [] => anyhow::bail!("no Firm with that name is visible to this caller"),
        _ => anyhow::bail!(
            "more than one visible Firm has that name; ask an administrator to disambiguate"
        ),
    };
    if let Some(brand) = brand.filter(|brand| !firm.brands.iter().any(|key| key == brand)) {
        // A brand key belongs to at most one Firm, so name which case this is
        // rather than one undifferentiated refusal (LAW-255).
        let worn = if firm.brands.is_empty() {
            "no brand".to_string()
        } else {
            firm.brands.join(", ")
        };
        let wearer = firms
            .iter()
            .find(|other| other.brands.iter().any(|key| key == brand));
        match wearer {
            Some(other) => anyhow::bail!(
                "brand `{brand}` is worn by a different Firm, `{}`; `{}` wears: {worn}",
                other.name,
                firm.name
            ),
            None => anyhow::bail!(
                "no visible Firm wears brand `{brand}`: the key is unknown or belongs to a Firm \
                 outside this caller's directory; `{}` wears: {worn}",
                firm.name
            ),
        }
    }
    Ok(firm.id)
}

/// `PATCH /app/api/projects/{id}` with `fields`, for the coordinates
/// `navigator project set` records on the live row. Returns the value of
/// [`store::project_surfaces::DRIVE_FOLDER_CHECK_HEADER`] when the server
/// checked a Drive folder named by the patch.
pub async fn patch_project_fields(
    host: &str,
    project_code: &str,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<Option<String>> {
    let (base, token) = resolve(Some(host))?;
    let project_id =
        crate::projects::surfaces::resolve_project_id(&base, &token, project_code).await?;
    let response = reqwest::Client::new()
        .patch(format!("{base}/app/api/projects/{project_id}"))
        .bearer_auth(&token)
        .json(fields)
        .send()
        .await
        .context("PATCH /app/api/projects/{id}")?;
    let status = response.status();
    let drive_folder_check = response
        .headers()
        .get(store::project_surfaces::DRIVE_FOLDER_CHECK_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "update live Project failed: {status}: {}",
            body.lines().next().unwrap_or_default().trim()
        ));
    }
    Ok(drive_folder_check)
}

/// `navigator site notation create <template-code> --project <code> --client-email …`
/// — open a notation on an **already-existing** matter and surface the
/// notation id. Every notation hangs on a pre-existing Project (the matter
/// is a deliberate prior step, `navigator project create`), so
/// `--project` is required: this resolves the human-facing matter **code**
/// to the Project id, then posts to the project-scoped create route
/// (`POST /app/projects/<project-code>/notations/new`). The template is read
/// from the Project's git repo when authored there, else from the bundled
/// firm catalog. Leaves the questionnaire ready for the site intake flow.
///
/// `answers_from` names a saved answers file (`site notation answers
/// --save-answers`): after the notation opens, every question the
/// questionnaire reaches is answered from the file, so the notation's
/// interpolated fields match the notation the file was saved from. A
/// question the file does not answer fails the create — prefill is whole,
/// never partial.
pub async fn notation_create(
    host: Option<&str>,
    template: &str,
    client_email: &str,
    project_code: &str,
    answers_from: Option<&Path>,
) -> ExitCode {
    run(async {
        let (base, token) = resolve(host)?;
        // No-redirect client so we read the 303 `Location` (the step URL)
        // rather than following it into the walker's HTML.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build http client")?;
        let url = format!("{base}/app/projects/{project_code}/notations/new");
        let form: Vec<(&str, &str)> =
            vec![("template_code", template), ("client_email", client_email)];
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .form(&form)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if status.as_u16() != 303 {
            // The walker re-renders its form (200) on a bad email/template;
            // anything but the redirect means no notation was created.
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "notation create did not start the questionnaire (status {status}). \
                 The server reported: {}",
                first_line(&body),
            ));
        }
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let notation_id: Uuid = location
            .trim_start_matches("/app/lawyer/notations/")
            .trim_end_matches("/step")
            .parse()
            .context("parse the notation id from the create redirect")?;
        println!(
            "{} {}",
            palette::dim("created notation; questionnaire ready — notation"),
            palette::highlight(notation_id.to_string()),
        );
        if let Some(path) = answers_from {
            let saved = crate::answers_file::load_answers_file(path)?;
            prefill_from_saved_answers(&client, &base, &token, notation_id, &saved).await?;
        }
        println!(
            "{}",
            palette::dim(format!(
                "continue this notation in the live site; notation id: {notation_id}"
            )),
        );
        Ok(())
    })
    .await
}

/// Answer a notation's questionnaire entirely from a saved answers file:
/// walk the same `/app/lawyer/notations/:id/step` route the intake walker
/// uses, posting each question's saved `state: value` as the answer.
/// Scalar saved answers post as the `value` field; a candidate-backed
/// question accepts its saved list number or row id. Aggregate questions
/// (`people_list`, schedules) have no single scalar and are refused — they
/// are answered interactively. A question the file does not answer fails
/// the walk, so a prefilled notation is always whole.
async fn prefill_from_saved_answers(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    notation_id: Uuid,
    saved: &crate::answers_file::SavedAnswers,
) -> Result<()> {
    let mut answered = 0u32;
    loop {
        let step = fetch_step(client, base, token, notation_id).await?;
        let Some(question) = step.question else {
            println!(
                "{} {} questions prefilled from the answers file",
                palette::dim("prefilled"),
                palette::highlight(answered.to_string()),
            );
            return Ok(());
        };
        let Some(value) = saved.get(&question.code) else {
            return Err(anyhow!(
                "the answers file has no answer for question `{}` ({}); prefill covers every \
                 question — save a complete notation's answers or answer `{}` interactively",
                question.code,
                question.prompt,
                question.code,
            ));
        };
        let fields = if store::question_registry::answer_type_is_aggregate(&question.answer_type) {
            return Err(anyhow!(
                "question `{}` is an aggregate ({}); a saved answer cannot fill it — answer it \
                 interactively",
                question.code,
                question.answer_type,
            ));
        } else if question.has_candidates() {
            picker_selection_fields(&question, &crate::answers_file::saved_scalar(value)?)?
        } else {
            vec![(
                "value".to_string(),
                crate::answers_file::saved_scalar(value)?,
            )]
        };
        let resp = client
            .post(format!("{base}/app/lawyer/notations/{notation_id}/step"))
            .bearer_auth(token)
            .form(&fields)
            .send()
            .await
            .context("POST step")?;
        let status = resp.status();
        if !status.is_success() && !status.is_redirection() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "prefilling `{}` failed: {}",
                question.code,
                server_error(status, &body),
            ));
        }
        println!(
            "{} {}",
            palette::dim("prefilled"),
            palette::highlight(&question.code)
        );
        answered += 1;
    }
}

/// Server response to a notation-draft POST: the draft's path on the
/// deployment, plus the notation run the push opened (LAW-203). Joined
/// with `base` to print and open the full URLs.
#[derive(Debug, Deserialize)]
struct NotationDraftCreated {
    id: Uuid,
    path: String,
    /// The run the push opened, when it opened one.
    notation_id: Option<Uuid>,
}

/// `navigator notation preview <file>` (LAW-29) — push a template as a
/// **draft** to the Project it belongs to: `POST
/// /app/projects/{project_code}/notations/draft`. The push persists and
/// validates the pushed source, enforces the engagement-first rule, and
/// opens a real notation run — the server returns the draft's path and
/// the run's (see `portal::notation_draft`). `code` is the template's file
/// stem, which the server stores and lints it under. Returns the draft's
/// id, the run's id when one opened, and the draft's full URL on the
/// deployment.
pub(crate) async fn create_notation_draft(
    host: Option<&str>,
    project_code: &str,
    slug: &str,
    code: &str,
    source: &str,
) -> Result<(Uuid, Option<Uuid>, String)> {
    let (base, token) = resolve(host)?;
    let client = reqwest::Client::new();
    let url = format!("{base}/app/projects/{project_code}/notations/draft");
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "slug": slug, "code": code, "source": source }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(draft_push_refusal(status, &body)));
    }
    let created: NotationDraftCreated =
        resp.json().await.context("parse notation-draft response")?;
    Ok((
        created.id,
        created.notation_id,
        format!("{base}{}", created.path),
    ))
}

/// The message for a refused draft push. A 422 is the server's verdict on
/// the template and lists every blocking violation one per line, so the
/// whole body is the answer; any other status keeps to its first line.
fn draft_push_refusal(status: reqwest::StatusCode, body: &str) -> String {
    let reported = if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        let lines: Vec<&str> = body
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() {
            first_line(body)
        } else {
            lines.join("\n")
        }
    } else {
        first_line(body)
    };
    format!("could not push a draft (status {status}). The server reported: {reported}")
}

/// `navigator site notation list --project <code>` — inspect the private
/// notation inventory for one matter without opening its browser workbench.
pub async fn notation_list(host: Option<&str>, project_code: &str, json: bool) -> ExitCode {
    run(async {
        let rows = fetch_notation_board(host, project_code).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            print_notation_board(&rows);
        }
        Ok(())
    })
    .await
}

/// `navigator project notations` — the manifest-scoped workflow board and its
/// pre-flight status. Exit non-zero when a template has never run or a review
/// state has exceeded the requested age.
pub async fn project_notations(
    host: Option<&str>,
    project_code: &str,
    stale_after: chrono::Duration,
    json: bool,
) -> ExitCode {
    run(async {
        let rows = fetch_notation_board(host, project_code).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            print_notation_board(&rows);
        }
        let now = chrono::Utc::now();
        let stale = rows.iter().any(|row| {
            let Some(state) = row.current_state.as_deref() else {
                return false;
            };
            if !(state.starts_with("reask__client") || state.starts_with("lawyer_review")) {
                return false;
            }
            row.last_transition_at
                .as_deref()
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                .is_none_or(|entered| now.signed_duration_since(entered) > stale_after)
        });
        let never_started = rows.iter().any(|row| row.notation_id.is_none());
        let stale_template = rows.iter().any(|row| row.template_stale);
        if stale || never_started || stale_template {
            return Err(anyhow!(
                "notation pre-flight found never-started work, stale review states, or runs whose template changed on main"
            ));
        }
        Ok(())
    })
    .await
}

async fn fetch_notation_board(
    host: Option<&str>,
    project_code: &str,
) -> Result<Vec<webapp::lawyer_project_notations::ProjectNotationRow>> {
    let client = DocumentClient::connect(host, project_code).await?;
    let url = format!(
        "{}/app/api/projects/{}/notation-inventory",
        client.base, client.project_id
    );
    let response = client
        .client
        .get(&url)
        .bearer_auth(&client.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "list notation inventory failed: {}",
            server_error(status, &body)
        ));
    }
    serde_json::from_str(&body).context("parse notation inventory")
}

/// Read the notation board for a matter, for repository-root Project workflows.
pub async fn notation_rows(
    host: Option<&str>,
    project_code: &str,
) -> Result<Vec<webapp::lawyer_project_notations::ProjectNotationRow>> {
    fetch_notation_board(host, project_code).await
}

fn print_notation_board(rows: &[webapp::lawyer_project_notations::ProjectNotationRow]) {
    let mut states = Vec::new();
    for row in rows {
        for state in &row.workflow_states {
            if !states.contains(state) {
                states.push(state.clone());
            }
        }
    }
    let mut table = Table::new();
    table.load_style(UTF8_FULL);
    table
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(states.iter().cloned().chain([
            "TEMPLATE".to_string(),
            "NOTATION UUID".to_string(),
            "CLIENT EMAIL".to_string(),
            "SIGNATURE REQUEST".to_string(),
            "LAST".to_string(),
            "STALE".to_string(),
        ]));
    for row in rows {
        let mut cells = Vec::with_capacity(states.len() + 6);
        for state in &states {
            if !row.workflow_states.contains(state) {
                cells.push("—".to_string());
            } else if row.notation_id.is_none() {
                cells.push("never started".to_string());
            } else if row.current_state.as_deref() == Some(state) {
                cells.push(format!(
                    "● {}",
                    row.state_entered_at.get(state).map_or("", String::as_str)
                ));
            } else if let Some(time) = row.state_entered_at.get(state) {
                cells.push(format!("✓ {time}"));
            } else {
                cells.push("·".to_string());
            }
        }
        cells.push(row.template_code.clone());
        cells.push(
            row.notation_id
                .clone()
                .unwrap_or_else(|| "never started".to_string()),
        );
        cells.push(
            row.respondent_email
                .clone()
                .unwrap_or_else(|| "—".to_string()),
        );
        cells.push(
            row.signature_request_id
                .clone()
                .unwrap_or_else(|| "—".to_string()),
        );
        cells.push(
            row.last_transition_at
                .clone()
                .unwrap_or_else(|| "—".to_string()),
        );
        cells.push(if row.template_stale {
            "template changed on main".to_string()
        } else {
            "—".to_string()
        });
        table.add_row(cells);
    }
    println!("{table}");
}

#[derive(Debug, Deserialize, Serialize)]
struct NotationAnswer {
    id: Uuid,
    question_id: Uuid,
    state_name: Option<String>,
    value: serde_json::Value,
    source: String,
    authored_by_person_id: Option<Uuid>,
}

/// `navigator site notation answers <id>` — read filed answers and their
/// provenance through the same lawyer-tier, matter-scoped API door as the
/// browser workbench. The door requires a firm-side participation row of every
/// tier, so an operator holding Owner or Admin and no row on the matter is
/// refused with the same `404` as anyone else.
///
/// `save_answers` persists the filed answers to a `state: value` YAML file
/// for `--answers-from` / `--answers-file` prefill — the save half of the
/// answer round trip.
pub async fn notation_answers(
    host: Option<&str>,
    notation_id: Uuid,
    json: bool,
    save_answers: Option<&Path>,
) -> ExitCode {
    run(async {
        let (base, token) = resolve(host)?;
        let url = format!("{base}/app/api/notations/{notation_id}/answers");
        let response = reqwest::Client::new()
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "read notation answers failed: {}",
                server_error(status, &body)
            ));
        }
        let answers: Vec<NotationAnswer> =
            serde_json::from_str(&body).context("parse notation answers")?;
        if json {
            println!("{}", serde_json::to_string_pretty(&answers)?);
        } else if answers.is_empty() {
            println!("{}", palette::dim("no filed answers"));
        } else {
            for answer in &answers {
                let state = answer.state_name.as_deref().unwrap_or("unscoped");
                let author = answer
                    .authored_by_person_id
                    .map_or_else(|| "system".to_string(), |id| id.to_string());
                println!("{state}: {} [{} by {author}]", answer.value, answer.source);
            }
        }
        if let Some(path) = save_answers {
            let filed: Vec<crate::answers_file::FiledAnswer> = answers
                .iter()
                .map(|a| crate::answers_file::FiledAnswer {
                    state_name: a.state_name.clone(),
                    value: a.value.clone(),
                })
                .collect();
            let saved = crate::answers_file::save_answers_file(path, &filed)?;
            println!(
                "{} {} answers to {}",
                palette::dim("saved"),
                palette::highlight(saved.to_string()),
                path.display(),
            );
        }
        Ok(())
    })
    .await
}

/// `navigator site notation approve <id>` — render + park the notation's
/// document (`POST …/approve-send`): fill the bound packet (a formation's
/// official Secretary-of-State form, or a retainer PDF) for attorney review.
/// Idempotent server-side — a re-approve once the PDF exists is a success,
/// not an error.
pub async fn notation_approve(host: Option<&str>, notation_id: Uuid) -> ExitCode {
    run(async {
        let (base, token) = resolve(host)?;
        let resp = reqwest::Client::new()
            .post(format!(
                "{base}/app/lawyer/notations/{notation_id}/approve-send"
            ))
            .bearer_auth(&token)
            // `Content-Length: 0` — the LB rejects an empty body without an
            // explicit length.
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body(Vec::<u8>::new())
            .send()
            .await
            .context("POST approve-send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("approve failed: {}", server_error(status, &body)));
        }
        let st = fetch_status(&base, &token, notation_id).await?;
        println!(
            "{} {} — state {} (document_ready {})",
            palette::dim("approved — notation"),
            palette::highlight(notation_id.to_string()),
            palette::highlight(&st.state),
            st.document_ready,
        );
        Ok(())
    })
    .await
}

/// `navigator site notation request-changes <id> --question <code> [--note …]`
/// — send a notation parked at `lawyer_review` back for changes. Records the
/// flagged question codes + note and routes `lawyer_review -> reask__client`
/// so a rejected review re-collects the wrong answers instead of dead-ending
/// (`POST …/request-changes`). Declining the matter outright is a separate
/// action; this one loops back to review after the answers are corrected.
pub async fn notation_request_changes(
    host: Option<&str>,
    notation_id: Uuid,
    questions: &[String],
    note: Option<&str>,
) -> ExitCode {
    run(async {
        if questions.is_empty() {
            return Err(anyhow!(
                "pass at least one --question <code> to flag for re-collection"
            ));
        }
        let (base, token) = resolve(host)?;
        // Dynamic checkbox-style fields: `q:<code>=on` per flagged question,
        // plus the optional reviewer note — the shape the web form posts.
        let mut form: Vec<(String, String)> = questions
            .iter()
            .map(|code| (format!("q:{code}"), "on".to_string()))
            .collect();
        if let Some(note) = note {
            form.push(("note".to_string(), note.to_string()));
        }
        let resp = no_redirect_client()?
            .post(format!(
                "{base}/app/lawyer/notations/{notation_id}/request-changes"
            ))
            .bearer_auth(&token)
            .form(&form)
            .send()
            .await
            .context("POST request-changes")?;
        let status = resp.status();
        if !(status.is_success() || status.is_redirection()) {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "request-changes failed: {}",
                server_error(status, &body)
            ));
        }
        println!(
            "{} {} — flagged {} for re-collection",
            palette::dim("sent back for changes — notation"),
            palette::highlight(notation_id.to_string()),
            palette::highlight(questions.join(", ")),
        );
        Ok(())
    })
    .await
}

/// `navigator site notation update <id> --answer <code=value>` — re-collect the
/// flagged answers on a notation parked at `reask__client` (lawyer on the
/// client's behalf) and resubmit for review (`POST …/reask`). The write is
/// gated server-side to the flagged set, and the corrected answers are what
/// the attorney re-reviews — answers and questions stay decoupled, so a
/// correction never re-walks the whole questionnaire.
pub async fn notation_update(
    host: Option<&str>,
    notation_id: Uuid,
    answers: &[String],
) -> ExitCode {
    run(async {
        if answers.is_empty() {
            return Err(anyhow!(
                "pass at least one --answer <code=value> to re-collect"
            ));
        }
        let mut form: Vec<(String, String)> = Vec::with_capacity(answers.len());
        for raw in answers {
            let (code, value) = raw
                .split_once('=')
                .ok_or_else(|| anyhow!("--answer must be `code=value`, got `{raw}`"))?;
            form.push((format!("a:{code}"), value.to_string()));
        }
        let (base, token) = resolve(host)?;
        let resp = no_redirect_client()?
            .post(format!("{base}/app/lawyer/notations/{notation_id}/reask"))
            .bearer_auth(&token)
            .form(&form)
            .send()
            .await
            .context("POST reask")?;
        let status = resp.status();
        if !(status.is_success() || status.is_redirection()) {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("update failed: {}", server_error(status, &body)));
        }
        let st = fetch_status(&base, &token, notation_id).await?;
        println!(
            "{} {} — state {}",
            palette::dim("re-collected and resubmitted — notation"),
            palette::highlight(notation_id.to_string()),
            palette::highlight(&st.state),
        );
        Ok(())
    })
    .await
}

/// `navigator site notation document <id> --out <path>` — download the
/// notation's rendered document (the filled packet) to a local file via
/// the participation-gated `…/documents/document` route, the same per-
/// notation PDF the review surface shows.
pub async fn notation_document(host: Option<&str>, notation_id: Uuid, out: &Path) -> ExitCode {
    run(async {
        let (base, token) = resolve(host)?;
        // Follow redirects: a signed-URL storage backend 302s to the blob;
        // the FsStorage dev backend streams 200 through the app.
        let resp = reqwest::Client::new()
            .get(format!(
                "{base}/app/lawyer/notations/{notation_id}/documents/document"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .context("GET document")?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Err(anyhow!(
                "no rendered document for notation {notation_id} — answer the questionnaire \
                 (or `navigator site notation approve {notation_id}`) first"
            ));
        }
        if !status.is_success() {
            return Err(anyhow!("document download failed: {status}"));
        }
        let bytes = resp.bytes().await.context("read document bytes")?;
        std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
        println!(
            "{} {} ({} bytes)",
            palette::dim("wrote the filled packet to"),
            palette::highlight(out.display().to_string()),
            bytes.len(),
        );
        Ok(())
    })
    .await
}

/// `navigator site notation status <id>` — print the workflow state + the
/// signature request id from the review handler's JSON branch.
pub async fn notation_status(host: Option<&str>, notation_id: Uuid, json: bool) -> ExitCode {
    run(async {
        let (base, token) = resolve(host)?;
        let st = fetch_status(&base, &token, notation_id).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&st)?);
        } else {
            println!(
                "{} state {}{} (delivery {}, document_ready {})",
                palette::dim(format!("notation {notation_id}")),
                palette::highlight(&st.state),
                st.signature_request_id
                    .as_deref()
                    .map(|id| format!(", signature request {id}"))
                    .unwrap_or_default(),
                st.delivery.as_deref().unwrap_or("—"),
                st.document_ready,
            );
        }
        Ok(())
    })
    .await
}

/// The review handler's `?format=json` body.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct NotationStatus {
    state: String,
    #[serde(default)]
    signature_request_id: Option<String>,
    #[serde(default)]
    delivery: Option<String>,
    /// Whether the worker has rendered + persisted the document PDF — the
    /// gate a client-side send must honor. Defaults to `false` for an older
    /// server that doesn't yet emit the field.
    #[serde(default)]
    document_ready: bool,
}

/// Render a non-2xx server response into an actionable line. The app's
/// error routes answer with a JSON `{error, reason}` body (the council's
/// "no opaque 500" point); fall back to the first line of a plain-text
/// body when the response isn't that shape.
fn server_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let error = v.get("error").and_then(serde_json::Value::as_str);
        let reason = v.get("reason").and_then(serde_json::Value::as_str);
        match (error, reason) {
            (Some(e), Some(r)) => return format!("{status}: {e} — {r}"),
            (_, Some(r)) => return format!("{status}: {r}"),
            (Some(e), _) => return format!("{status}: {e}"),
            _ => {}
        }
    }
    format!("{status}: {}", first_line(body))
}

/// One step of the questionnaire walker's `?format=json` body.
#[derive(Debug, serde::Deserialize)]
struct StepResponse {
    /// The next question, or `None` once the questionnaire reaches END.
    #[serde(default)]
    question: Option<StepQuestion>,
}

/// The question metadata the walker shows for one step.
#[derive(Debug, serde::Deserialize)]
struct StepQuestion {
    code: String,
    prompt: String,
    answer_type: String,
    /// DB-backed picker candidates for singular record/reference
    /// questions, empty for primitive and aggregate answers.
    #[serde(default)]
    candidates: Vec<StepCandidate>,
}

impl StepQuestion {
    fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }
}

#[derive(Debug, serde::Deserialize)]
struct StepCandidate {
    id: Uuid,
}

/// GET the current questionnaire step as JSON.
async fn fetch_step(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    notation_id: Uuid,
) -> Result<StepResponse> {
    let resp = client
        .get(format!(
            "{base}/app/lawyer/notations/{notation_id}/step?format=json"
        ))
        .bearer_auth(token)
        .send()
        .await
        .context("GET step (json)")?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(anyhow!("no notation {notation_id} on {base}"));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("step failed: {}", server_error(status, &body)));
    }
    resp.json::<StepResponse>().await.context("parse step json")
}

fn picker_selection_fields(question: &StepQuestion, raw: &str) -> Result<Vec<(String, String)>> {
    let selected = select_candidate(question, raw.trim())?;
    Ok(vec![("id".to_string(), selected.id.to_string())])
}

fn select_candidate<'a>(question: &'a StepQuestion, raw: &str) -> Result<&'a StepCandidate> {
    if raw.is_empty() {
        return Err(anyhow!(
            "question `{}` needs a picker selection; enter a list number or row id",
            question.code
        ));
    }
    if let Ok(n) = raw.parse::<usize>() {
        return question.candidates.get(n.saturating_sub(1)).ok_or_else(|| {
            anyhow!(
                "selection {n} is out of range for question `{}` ({} option{})",
                question.code,
                question.candidates.len(),
                if question.candidates.len() == 1 {
                    ""
                } else {
                    "s"
                },
            )
        });
    }
    if let Ok(id) = Uuid::parse_str(raw) {
        return question
            .candidates
            .iter()
            .find(|candidate| candidate.id == id)
            .ok_or_else(|| {
                anyhow!(
                    "selection id {id} is not listed for question `{}`",
                    question.code
                )
            });
    }
    Err(anyhow!(
        "selection `{raw}` for question `{}` is not a list number or row id",
        question.code
    ))
}

async fn fetch_status(base: &str, token: &str, notation_id: Uuid) -> Result<NotationStatus> {
    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/app/lawyer/notations/{notation_id}/review?format=json"
        ))
        .bearer_auth(token)
        .send()
        .await
        .context("GET notation review (json)")?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(anyhow!("no notation {notation_id} on {base}"));
    }
    if !status.is_success() {
        return Err(anyhow!("notation status failed: {status}"));
    }
    resp.json::<NotationStatus>()
        .await
        .context("parse notation status json")
}

/// First non-empty line of a response body, trimmed — for terse error
/// reporting without dumping a whole HTML page.
/// What one failed integrity page attempt reports. A gateway status whose
/// body is the gateway's own HTML page reads as the timeout it is, and a
/// request the CLI abandoned names its limit; any other failure keeps the
/// first line of the server's answer.
fn integrity_failure(
    outcome: std::result::Result<(reqwest::StatusCode, &str), &reqwest::Error>,
    limit: std::time::Duration,
) -> String {
    match outcome {
        Ok((status, body))
            if matches!(status.as_u16(), 502..=504) && body.trim_start().starts_with('<') =>
        {
            format!(
                "{status}: the gateway timed out or dropped the request before the server answered"
            )
        }
        Ok((status, body)) => format!("{status}: {}", first_line(body)),
        Err(error) if error.is_timeout() => format!(
            "timed out after {}s waiting for the server ({error})",
            limit.as_secs()
        ),
        Err(error) => error.to_string(),
    }
}

fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(empty response)")
        .chars()
        .take(200)
        .collect()
}

/// Tries before the project lookup gives up on a gateway error.
const PROJECT_LOOKUP_ATTEMPTS: u32 = 4;
const PROJECT_LOOKUP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// `GET /app/api/projects` with the retry every project-code lookup shares.
/// The read is idempotent, so a 502, 503, 504, connect failure, or timeout is
/// retried with a growing delay; any other status (401, 404, ...) returns at
/// once. A 401 is a [`UploadSessionExpired`]. Returns the response body.
pub(crate) async fn fetch_project_list(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<String> {
    fetch_project_list_with(
        client,
        base,
        token,
        PROJECT_LOOKUP_ATTEMPTS,
        PROJECT_LOOKUP_RETRY_DELAY,
    )
    .await
}

/// Every Project the stored login can see, as `(id, code)` pairs — the
/// intake verbs' matter enumeration (`navigator downloads propose` and
/// `clean`).
pub(crate) async fn visible_projects(host: Option<&str>) -> Result<Vec<(Uuid, String)>> {
    let (base, token) = resolve(host)?;
    visible_projects_on(&base, &token).await
}

/// The same enumeration for a caller that already holds a login — `clean`
/// walks every matter with one login instead of re-resolving per project.
pub(crate) async fn visible_projects_on(base: &str, token: &str) -> Result<Vec<(Uuid, String)>> {
    let client = authenticated_client_builder(base)?
        .build()
        .context("build document transfer client")?;
    let body = fetch_project_list(&client, base, token).await?;
    let projects: Vec<VisibleProject> =
        serde_json::from_str(&body).context("parse GET /app/api/projects")?;
    Ok(projects
        .into_iter()
        .map(|project| (project.id, project.code))
        .collect())
}

async fn fetch_project_list_with(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    attempts: u32,
    delay: std::time::Duration,
) -> Result<String> {
    for attempt in 1..=attempts {
        let failure;
        let retryable = match client
            .get(format!("{base}/app/api/projects"))
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if status.is_success() {
                    return Ok(body);
                }
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    return Err(UploadSessionExpired(base.to_string()).into());
                }
                failure = format!("{status}: {}", trimmed_body(&body));
                matches!(status.as_u16(), 502..=504)
            }
            Err(error) => {
                failure = error.to_string();
                error.is_timeout() || error.is_connect()
            }
        };
        if !retryable || attempt == attempts {
            return Err(anyhow!(
                "list projects failed: {failure} (attempt {attempt} of {attempts})"
            ));
        }
        tokio::time::sleep(delay * attempt).await;
    }
    unreachable!("the final attempt returns")
}

/// A response body as one short line: markup tags dropped, whitespace
/// collapsed, capped at 200 characters.
fn trimmed_body(body: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return "(empty response)".to_string();
    }
    line.chars().take(200).collect()
}

/// Drive an async fallible command to an `ExitCode`, printing any error.
/// [`exit_code_for`] distinguishes a CI mint refusal (exit `3`) from every
/// other failure (the ordinary gate-failure exit `2`).
async fn run<F>(fut: F) -> ExitCode
where
    F: std::future::Future<Output = Result<()>>,
{
    match fut.await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("navigator: {e:#}");
            exit_code_for(&e)
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;
    use std::sync::LazyLock;

    use super::move_local_pointer;
    use super::reslug_with_local;
    use super::retire_document;
    use super::{
        archive_repository, create_notation_draft, document_reclassify, document_repair,
        document_slug, document_transcribe_from, document_upload, document_upload_from,
        document_visibility, fetch_status, mail_file, matter_close, notation_answers,
        notation_approve, notation_create, notation_document, notation_list,
        notation_request_changes, notation_status, notation_update, picker_selection_fields,
        project_code_exists, projects_create, projects_create_named, seed, seed_directory,
        select_candidate, source_revision, DocumentClient, DocumentExecution, RevisionSummary,
        SeedCredential, SeedOptions, StepResponse,
    };
    use super::{
        exit_code_for, fetch_step, first_line, integrity_failure, mint_refusal_annotation,
        preflight_dns, server_error,
    };
    use super::{fetch_project_list_with, trimmed_body, UploadSessionExpired};
    use crate::credentials::{self, Credentials, HostCredential};
    use uuid::Uuid;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static CREDENTIALS_ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    struct CredentialsEnv {
        _dir: tempfile::TempDir,
        previous: Option<String>,
    }

    #[derive(Clone, Copy)]
    struct LawyerRouteIds {
        notation: Uuid,
        project: Uuid,
    }

    impl CredentialsEnv {
        fn new(base: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("navigator.json");
            let mut creds = Credentials::default();
            creds.set(
                base,
                HostCredential {
                    token: "test-token".into(),
                    person_email: Some("lawyer@neonlaw.com".into()),
                    role: Some("lawyer".into()),
                    expires_at: super::now_secs() + 3600,
                },
            );
            credentials::save(&path, &creds).unwrap();
            let previous = std::env::var("NAVIGATOR_CREDENTIALS_FILE").ok();
            std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", path);
            Self {
                _dir: dir,
                previous,
            }
        }
    }

    #[tokio::test]
    async fn firm_resolution_checks_brand_and_authorized_directory() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app/api/firms"))
            .and(wiremock::matchers::header("authorization", "Bearer test-token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id":"00000000-0000-0000-0000-000000000001", "name":"Example Practice", "brands":["sample"]}
            ]))).mount(&server).await;
        let id = super::resolve_firm(
            &server.uri(),
            "test-token",
            "example practice",
            Some("sample"),
        )
        .await
        .unwrap();
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000001");
        assert!(
            super::resolve_firm(&server.uri(), "test-token", &id.to_string(), Some("sample"))
                .await
                .is_err()
        );
        assert!(super::resolve_firm(
            &server.uri(),
            "test-token",
            "Example Practice",
            Some("other")
        )
        .await
        .is_err());
        assert!(
            super::resolve_firm(&server.uri(), "test-token", "hidden", Some("sample"))
                .await
                .is_err()
        );
    }

    /// LAW-255: a refused brand names its case — a key another visible Firm
    /// wears, or a key no visible Firm wears — and lists the selected Firm's
    /// own keys.
    #[tokio::test]
    async fn firm_resolution_names_why_a_brand_is_refused() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app/api/firms"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id":"00000000-0000-0000-0000-000000000001", "name":"Example Practice", "brands":["sample", "second"]},
                {"id":"00000000-0000-0000-0000-000000000002", "name":"Other Practice", "brands":["elsewhere"]}
            ]))).mount(&server).await;
        let refusal = |brand: &'static str| {
            let base = server.uri();
            async move {
                super::resolve_firm(&base, "test-token", "Example Practice", Some(brand))
                    .await
                    .unwrap_err()
                    .to_string()
            }
        };
        assert_eq!(
            refusal("elsewhere").await,
            "brand `elsewhere` is worn by a different Firm, `Other Practice`; \
             `Example Practice` wears: sample, second"
        );
        assert_eq!(
            refusal("no-such-brand").await,
            "no visible Firm wears brand `no-such-brand`: the key is unknown or belongs to a Firm \
             outside this caller's directory; `Example Practice` wears: sample, second"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_reports_an_expired_stored_token() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("navigator.json");
        let base = "https://live.example.com";
        let mut creds = Credentials::default();
        creds.set(
            base,
            HostCredential {
                token: "stale-token".into(),
                person_email: Some("lawyer@neonlaw.com".into()),
                role: Some("lawyer".into()),
                expires_at: super::now_secs() - 1,
            },
        );
        credentials::save(&path, &creds).unwrap();
        let previous = std::env::var("NAVIGATOR_CREDENTIALS_FILE").ok();
        std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", &path);

        let err = super::resolve(Some(base)).unwrap_err();

        match previous {
            Some(p) => std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", p),
            None => std::env::remove_var("NAVIGATOR_CREDENTIALS_FILE"),
        }

        assert!(
            err.to_string().contains("expired"),
            "expected an expired-token error, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_notation_draft_posts_the_slug_and_source_and_joins_the_returned_path() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let draft_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();

        Mock::given(method("POST"))
            .and(path("/app/projects/acme/notations/draft"))
            .and(body_json(serde_json::json!({
                "slug": "sample-letter",
                "code": "sample_letter",
                "source": "# Sample Letter\n",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": draft_id,
                "path": format!("/notations/drafts/{draft_id}"),
                "notation_id": run_id,
                "run_path": format!("/app/lawyer/notations/{run_id}/step"),
            })))
            .expect(1)
            .mount(&server)
            .await;

        // The push hands back the run it opened (LAW-203) beside the
        // draft — the CLI prints it so the lawyer knows the preview is
        // backed by a real questionnaire.
        let (id, run, url) = create_notation_draft(
            Some(&server_uri),
            "acme",
            "sample-letter",
            "sample_letter",
            "# Sample Letter\n",
        )
        .await
        .expect("the draft is pushed");

        assert_eq!(id, draft_id);
        assert_eq!(run, Some(run_id));
        assert_eq!(url, format!("{server_uri}/notations/drafts/{draft_id}"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_notation_draft_reports_the_servers_refusal() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);

        Mock::given(method("POST"))
            .and(path("/app/projects/no-such-matter/notations/draft"))
            .respond_with(ResponseTemplate::new(404).set_body_string("matter not found"))
            .expect(1)
            .mount(&server)
            .await;

        let err = create_notation_draft(
            Some(&server_uri),
            "no-such-matter",
            "sample-letter",
            "sample_letter",
            "# Sample Letter\n",
        )
        .await
        .expect_err("a 404 is not a pushed draft");

        assert!(err.to_string().contains("matter not found"), "{err}");
    }

    /// LAW-274: a 422 names every rule the template broke, not only the
    /// summary line — the CLI reports the server's whole verdict.
    #[tokio::test(flavor = "current_thread")]
    async fn create_notation_draft_reports_every_violation_the_server_lists() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);

        Mock::given(method("POST"))
            .and(path("/app/projects/acme/notations/draft"))
            .respond_with(ResponseTemplate::new(422).set_body_string(
                "template `sample_letter` has 2 blocking rule violation(s); fix it and retry\n\
                 sample_letter.md:1 N101: Frontmatter is missing `title`\n\
                 sample_letter.md:9 S101: Line is 130 characters\n",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let err = create_notation_draft(
            Some(&server_uri),
            "acme",
            "sample-letter",
            "sample_letter",
            "# Sample Letter\n",
        )
        .await
        .expect_err("a 422 is not a pushed draft");

        let message = err.to_string();
        assert!(message.contains("status 422"), "{message}");
        assert!(
            message.contains("\nsample_letter.md:1 N101: Frontmatter is missing `title`"),
            "{message}"
        );
        assert!(
            message.contains("\nsample_letter.md:9 S101: Line is 130 characters"),
            "{message}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn seed_posts_dry_run_and_overwrite_flags() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Person.yaml");
        let yaml = "lookup_fields:\n  - email\nrecords: []\n";
        std::fs::write(&file, yaml).unwrap();

        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .and(body_json(serde_json::json!({
                "model": "person",
                "yaml": yaml,
                "overwrite": true,
                "dry_run": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "person",
                "created": 0,
                "updated": 0,
                "unchanged": 0,
                "records": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            seed(
                SeedCredential::Stored {
                    host: Some(server_uri),
                },
                "person",
                &file,
                SeedOptions {
                    overwrite: true,
                    dry_run: true,
                    as_ci_project: None,
                },
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn seed_ci_exchanges_github_oidc_then_posts_the_seed() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let navigator = MockServer::start().await;
        let navigator_uri = navigator.uri();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Person.yaml");
        let yaml = "lookup_fields:\n  - email\nrecords: []\n";
        std::fs::write(&file, yaml).unwrap();

        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");

        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("audience", navigator_uri.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "github-jwt"
            })))
            .expect(1)
            .mount(&github)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/seed-token"))
            .and(body_json(serde_json::json!({ "token": "github-jwt" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "navigator-ci-token",
                "exp": 1,
                "project_code": "acme"
            })))
            .expect(1)
            .mount(&navigator)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .and(body_json(serde_json::json!({
                "model": "person",
                "yaml": yaml,
                "overwrite": false,
                "dry_run": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "person",
                "created": 0,
                "updated": 0,
                "unchanged": 0,
                "records": [],
            })))
            .expect(1)
            .mount(&navigator)
            .await;

        let result = seed(
            SeedCredential::Ci {
                host: navigator_uri,
            },
            "person",
            &file,
            SeedOptions::default(),
        )
        .await;

        match previous_url {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        }
        match previous_token {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        }

        assert_eq!(result, ExitCode::SUCCESS);
    }

    /// LAW-9: a mint refused by `/auth/ci/seed-token` itself — the door said
    /// no before any gate ran — exits `3`, distinct from an ordinary
    /// gate-failure `2`, and `mint_ci_token` prints a GitHub `::error::`
    /// annotation naming the door and the door's own `message`.
    #[tokio::test(flavor = "current_thread")]
    async fn a_refused_seed_mint_exits_3_not_the_gate_failure_2() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let navigator = MockServer::start().await;
        let navigator_uri = navigator.uri();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Person.yaml");
        std::fs::write(&file, "lookup_fields:\n  - email\nrecords: []\n").unwrap();

        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");

        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("audience", navigator_uri.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "github-jwt"
            })))
            .expect(1)
            .mount(&github)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/seed-token"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": "forbidden",
                "message": "the live project has no lawyer-tier lawyer DRI to attribute a CI seed write to",
            })))
            .expect(1)
            .mount(&navigator)
            .await;

        let result = seed(
            SeedCredential::Ci {
                host: navigator_uri,
            },
            "person",
            &file,
            SeedOptions::default(),
        )
        .await;

        match previous_url {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        }
        match previous_token {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        }

        assert_eq!(result, ExitCode::from(3));
    }

    /// LAW-285: a transient 5xx from the runner's token service recovers —
    /// the OIDC fetch retries a 500, a gateway 502-504, a connect error, or
    /// a timeout instead of failing the run on the first attempt.
    #[tokio::test(flavor = "current_thread")]
    async fn an_oidc_token_fetch_survives_a_transient_5xx() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");

        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .expect(2)
            .mount(&github)
            .await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "github-jwt"
            })))
            .expect(1)
            .mount(&github)
            .await;

        let token = super::actions_oidc_token(
            &reqwest::Client::new(),
            "https://navigator.example",
            super::CiForge::GitHub,
        )
        .await
        .expect("the third attempt succeeds");
        assert_eq!(token, "github-jwt");

        match previous_url {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        }
        match previous_token {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        }
    }

    /// A 4xx from the token endpoint is a deterministic refusal — terminal on
    /// the first attempt, like every 4xx the gate sees.
    #[tokio::test(flavor = "current_thread")]
    async fn an_oidc_token_refusal_is_terminal_on_the_first_attempt() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");

        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&github)
            .await;

        let error = super::actions_oidc_token(
            &reqwest::Client::new(),
            "https://navigator.example",
            super::CiForge::GitHub,
        )
        .await
        .expect_err("a 403 is never retried");
        assert!(error.to_string().contains("403"), "{error:#}");

        match previous_url {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        }
        match previous_token {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        }
    }

    /// A transient 5xx from the mint door recovers: the third attempt mints
    /// the session the first two could not.
    #[tokio::test(flavor = "current_thread")]
    async fn a_transient_mint_status_recovers() {
        let navigator = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .expect(2)
            .mount(&navigator)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "ci-session",
                "project_code": "acme",
            })))
            .expect(1)
            .mount(&navigator)
            .await;

        let minted = super::mint_ci_token(
            &reqwest::Client::new(),
            &navigator.uri(),
            "/auth/ci/document-token",
            "oidc",
        )
        .await
        .expect("the third attempt succeeds");
        assert_eq!(minted.token, "ci-session");
    }

    /// A 403 refusal from the mint door stays terminal on the first attempt
    /// with the exit-3 `MintRefusal` vocabulary — retries never soften or
    /// multiply a deterministic refusal.
    #[tokio::test(flavor = "current_thread")]
    async fn a_refused_mint_stays_terminal_on_the_first_attempt() {
        let navigator = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": "forbidden",
                "message": "the run is not bound to a live project",
            })))
            .expect(1)
            .mount(&navigator)
            .await;

        let error = super::mint_ci_token(
            &reqwest::Client::new(),
            &navigator.uri(),
            "/auth/ci/document-token",
            "oidc",
        )
        .await
        .expect_err("a 403 refusal is never retried");
        assert_eq!(exit_code_for(&error), ExitCode::from(3));
        assert!(error.to_string().contains("403"), "{error:#}");
    }

    /// A mint that keeps 5xx-ing gives up after four attempts and reports
    /// the same exit-3 refusal it would have reported without the retry.
    /// Real clock: a paused clock auto-advances past the request timeout
    /// while the mock's response is still in flight, turning a 503 into a
    /// spurious transport timeout.
    #[tokio::test]
    async fn a_persistently_5xx_mint_gives_up_after_four_attempts() {
        let navigator = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .respond_with(ResponseTemplate::new(503))
            .expect(4)
            .mount(&navigator)
            .await;

        let error = super::mint_ci_token(
            &reqwest::Client::new(),
            &navigator.uri(),
            "/auth/ci/document-token",
            "oidc",
        )
        .await
        .expect_err("a door that never answers cannot mint");
        assert_eq!(
            exit_code_for(&error),
            ExitCode::from(3),
            "a persistent 5xx must end as a refusal, not a plain failure: {error:#}"
        );
    }

    /// The mint path's requests carry the 30 s bound: a hung endpoint burns
    /// an attempt instead of hanging the job. The clock is paused, so the
    /// four timed-out attempts and their delays pass without real waiting —
    /// the wall-clock assert below proves it.
    #[tokio::test(start_paused = true)]
    async fn a_hung_mint_times_out_instead_of_hanging() {
        let navigator = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
            .expect(4)
            .mount(&navigator)
            .await;

        let started = std::time::Instant::now();
        let error = super::mint_ci_token(
            &reqwest::Client::new(),
            &navigator.uri(),
            "/auth/ci/document-token",
            "oidc",
        )
        .await
        .expect_err("a hung endpoint cannot mint");
        let timed_out = error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        });
        assert!(timed_out, "{error:#}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the 30 s bound fired without real waiting"
        );
    }

    /// Every gate remote call carries an explicit request timeout — the
    /// configuration the hangs are not testable against, asserted here
    /// because reqwest does not expose a client's own timeout for reading.
    #[test]
    fn the_gate_remote_calls_carry_explicit_bounds() {
        assert_eq!(super::GATE_MINT_TIMEOUT, std::time::Duration::from_secs(30));
        assert_eq!(
            super::INTEGRITY_REQUEST_TIMEOUT,
            std::time::Duration::from_secs(100)
        );
    }

    /// A hung deployment fails a document-client request at its timeout
    /// instead of hanging, and the timeout is classified transient — the
    /// projects fetch exhausts its four attempts rather than dying on one.
    #[tokio::test(flavor = "current_thread")]
    async fn a_document_client_request_times_out_and_retries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {"id": Uuid::now_v7(), "code": "acme"}
                    ]))
                    .set_delay(std::time::Duration::from_millis(300)),
            )
            .expect(4)
            .mount(&server)
            .await;

        let error = DocumentClient::with_timeout(
            server.uri(),
            "test-token".into(),
            "acme",
            Some(std::time::Duration::from_millis(50)),
        )
        .await
        .expect_err("a 300 ms response cannot pass a 50 ms bound");
        assert!(error.to_string().contains("attempt 4 of 4"), "{error:#}");
    }

    /// A plain 500 on a revision read recovers — the read is idempotent, so
    /// the second attempt answers what the first could not.
    #[tokio::test(flavor = "current_thread")]
    async fn a_revision_read_recovers_from_a_transient_500() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let revisions_path = format!("/app/api/projects/{project_id}/documents/revisions");
        Mock::given(method("GET"))
            .and(path(revisions_path.as_str()))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(revisions_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "kind": "documents",
                "revisions": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let revisions = client
            .list_revisions("will")
            .await
            .expect("the second attempt succeeds");
        assert!(revisions.revisions.is_empty());
    }

    /// An absent revision is a deterministic refusal — terminal on the first
    /// attempt, with the message the check reports unchanged.
    #[tokio::test(flavor = "current_thread")]
    async fn a_missing_revision_is_terminal_on_the_first_attempt() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/revisions"
            )))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let error = client
            .list_revisions("absent")
            .await
            .expect_err("a 404 is never retried");
        assert!(
            error
                .to_string()
                .contains("no revision of `absent` is visible on this matter: 404"),
            "{error:#}"
        );
    }

    /// An integrity page retries a plain 500 like the gateway errors — the
    /// same read, one more recovered page.
    #[tokio::test(flavor = "current_thread")]
    async fn an_integrity_page_retries_a_plain_500() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let integrity_path = format!("/app/api/projects/{project_id}/documents/integrity");
        Mock::given(method("GET"))
            .and(path(integrity_path.as_str()))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(integrity_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "assets": [],
                "integrations": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let page = client
            .integrity(false)
            .await
            .expect("the second attempt succeeds");
        assert!(page.assets.is_empty());
    }

    /// Forgejo's runner also sets `GITHUB_ACTIONS=true`, so `FORGEJO_ACTIONS`
    /// alone selects the Forgejo door.
    #[test]
    fn forgejo_actions_selects_the_forgejo_document_door() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_string())
            }
        };
        let forgejo = super::CiForge::from_lookup(lookup(&[
            ("FORGEJO_ACTIONS", "true"),
            ("GITHUB_ACTIONS", "true"),
        ]));
        assert_eq!(forgejo, super::CiForge::Forgejo);
        assert_eq!(
            forgejo.document_mint_path(),
            "/auth/ci/forgejo/document-token"
        );
        let github = super::CiForge::from_lookup(lookup(&[("GITHUB_ACTIONS", "true")]));
        assert_eq!(github, super::CiForge::GitHub);
        assert_eq!(github.document_mint_path(), "/auth/ci/document-token");
        assert_eq!(
            super::CiForge::from_lookup(lookup(&[("FORGEJO_ACTIONS", "")])),
            super::CiForge::GitHub
        );
    }

    #[test]
    fn each_forge_has_its_own_seed_door() {
        assert_eq!(
            super::CiForge::GitHub.seed_mint_path(),
            "/auth/ci/seed-token"
        );
        assert_eq!(
            super::CiForge::Forgejo.seed_mint_path(),
            "/auth/ci/forgejo/seed-token"
        );
    }

    /// LAW-266: a `project.host` with no DNS record is named, not reported as
    /// a raw resolver error. `.invalid` never resolves (RFC 2606).
    #[tokio::test]
    async fn a_mint_against_an_unresolvable_host_names_the_host() {
        // No proxy: a runner's `HTTPS_PROXY` would answer for the host and
        // the test would stop exercising local DNS.
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = super::mint_ci_token(
            &client,
            "https://unpublished-brand-host.invalid",
            "/auth/ci/document-token",
            "oidc",
        )
        .await
        .expect_err("an .invalid host cannot resolve");
        let message = error.to_string();
        assert!(
            message.contains("project.host `unpublished-brand-host.invalid` does not resolve"),
            "{error:#}"
        );
        assert!(
            message.contains("POST /auth/ci/document-token"),
            "{error:#}"
        );
        assert_eq!(exit_code_for(&error), ExitCode::from(2));
    }

    /// LAW-266: the pre-flight resolves the serving host before any request
    /// leaves the runner — the OIDC audience request included — so an
    /// unresolvable `project.host` is refused at the earliest step with the
    /// host-naming remedies. No `ACTIONS_ID_TOKEN_REQUEST_URL` is staged: if
    /// the pre-flight failed to fire, this call would die on the missing
    /// grant environment instead of naming the host.
    #[tokio::test]
    async fn ci_document_resolution_refuses_an_unresolvable_host_before_the_oidc_request() {
        let error = super::resolve_ci_document_on("brand.invalid", super::CiForge::GitHub)
            .await
            .expect_err("an .invalid host cannot resolve");
        let message = format!("{error:#}");
        assert!(
            message.contains("project.host `brand.invalid` does not resolve"),
            "{message}"
        );
        assert!(
            message.contains("POST /auth/ci/document-token"),
            "{message}"
        );
        assert!(
            message.contains("Publish the host's DNS record"),
            "{message}"
        );
        assert!(
            !message.contains("ACTIONS_ID_TOKEN_REQUEST_URL"),
            "{message}"
        );
    }

    /// A loopback host resolves everywhere, so a local mock deployment never
    /// trips the pre-flight.
    #[test]
    fn the_dns_preflight_passes_a_loopback_host_without_a_refusal() {
        preflight_dns("http://127.0.0.1:9", "/auth/ci/document-token")
            .expect("loopback always resolves");
    }

    /// The classifier reads the resolver's own words, and only those: a plain
    /// connection refusal is a reachable host with a closed port, which the
    /// real request reports in its own words.
    #[test]
    fn the_dns_classifier_reads_the_resolver_words_only() {
        let miss = std::io::Error::other(
            "failed to lookup address information: Name or service not known",
        );
        assert!(super::error_chain_names_dns(&miss));

        let refused = std::io::Error::other("Connection refused (os error 111)");
        assert!(!super::error_chain_names_dns(&refused));
    }

    #[test]
    fn mint_refusal_annotation_names_the_door_and_the_servers_message() {
        assert_eq!(
            mint_refusal_annotation(
                "/auth/ci/document-token",
                "403 Forbidden: the live project has no lawyer-tier lawyer DRI to attribute CI document verification to",
            ),
            "::error::CI token mint at /auth/ci/document-token was refused: 403 Forbidden: \
             the live project has no lawyer-tier lawyer DRI to attribute CI document verification to"
        );
    }

    /// A DNS miss never got a server message, so the annotation carries the
    /// host-naming refusal instead of a status.
    #[test]
    fn mint_refusal_annotation_carries_the_dns_refusal_for_an_unresolvable_host() {
        let annotation = mint_refusal_annotation(
            "/auth/ci/forgejo/document-token",
            &super::unreachable_host_message(
                "https://brand.invalid",
                "POST /auth/ci/forgejo/document-token",
            ),
        );
        assert!(annotation.starts_with("::error::"), "{annotation}");
        assert!(
            annotation.contains("project.host `brand.invalid` does not resolve"),
            "{annotation}"
        );
        assert!(
            annotation.contains("/auth/ci/forgejo/document-token"),
            "{annotation}"
        );
    }

    #[test]
    fn exit_code_for_a_mint_refusal_is_3_and_every_other_error_is_2() {
        let refusal = anyhow::Error::new(super::MintRefusal)
            .context("CI token mint at /auth/ci/seed-token failed: 403 Forbidden: refused");
        assert_eq!(exit_code_for(&refusal), ExitCode::from(3));

        let gate_failure = anyhow::anyhow!("2 of 2 pointer(s) failed live verification");
        assert_eq!(exit_code_for(&gate_failure), ExitCode::from(2));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn seed_directory_imports_supported_stems_in_model_order() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let dir = tempfile::tempdir().unwrap();
        let person = "lookup_fields:\n  - email\nrecords: []\n";
        let roles = "lookup_fields:\n  - person_id\n  - project_id\nrecords: []\n";
        std::fs::write(dir.path().join("PersonProjectRole.yaml"), roles).unwrap();
        std::fs::write(dir.path().join("Person.yaml"), person).unwrap();

        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .and(body_json(serde_json::json!({
                "model": "person",
                "yaml": person,
                "overwrite": false,
                "dry_run": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "person",
                "created": 0,
                "updated": 0,
                "unchanged": 0,
                "records": [],
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .and(body_json(serde_json::json!({
                "model": "person_project_role",
                "yaml": roles,
                "overwrite": false,
                "dry_run": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "person_project_role",
                "created": 0,
                "updated": 0,
                "unchanged": 0,
                "records": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            seed_directory(
                SeedCredential::Stored {
                    host: Some(server_uri),
                },
                dir.path(),
                SeedOptions::default(),
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    /// A person seed of `count` records.
    fn people_yaml(count: usize) -> String {
        use std::fmt::Write as _;
        let mut yaml = String::from("lookup_fields:\n  - email\nrecords:\n");
        for index in 0..count {
            let _ = write!(
                yaml,
                "  - email: person{index}@example.com\n    name: Person {index}\n"
            );
        }
        yaml
    }

    fn seed_report(created: usize, unchanged: usize) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "person",
            "created": created,
            "updated": 0,
            "unchanged": unchanged,
            "records": [],
        }))
    }

    /// The number of records a posted seed chunk carries, and its dry-run flag.
    fn posted_chunk(request: &wiremock::Request) -> (usize, bool) {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let yaml = body["yaml"].as_str().unwrap();
        (yaml.matches("- email: ").count(), body["dry_run"] == true)
    }

    async fn import_people(server: &MockServer, count: usize) -> ExitCode {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Person.yaml");
        std::fs::write(&file, people_yaml(count)).unwrap();
        seed(
            SeedCredential::Stored {
                host: Some(server.uri()),
            },
            "person",
            &file,
            SeedOptions::default(),
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_large_seed_document_is_posted_in_chunks() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let _env = CredentialsEnv::new(&server.uri());
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with(|request: &wiremock::Request| {
                let (records, _) = posted_chunk(request);
                seed_report(records, 0)
            })
            .mount(&server)
            .await;

        assert_eq!(import_people(&server, 120).await, ExitCode::SUCCESS);
        let chunks: Vec<usize> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| posted_chunk(request).0)
            .collect();
        assert_eq!(chunks, vec![50, 50, 20]);
    }

    /// The proxy answers 502 but the server finished the write: the dry-run
    /// re-check reads every record unchanged, so the chunk counts as
    /// committed and the import succeeds.
    #[tokio::test(flavor = "current_thread")]
    async fn a_502_chunk_that_committed_anyway_succeeds() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let _env = CredentialsEnv::new(&server.uri());
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with(|request: &wiremock::Request| match posted_chunk(request) {
                (records, true) => seed_report(0, records),
                (_, false) => ResponseTemplate::new(502).set_body_string("Bad Gateway"),
            })
            .mount(&server)
            .await;

        assert_eq!(import_people(&server, 10).await, ExitCode::SUCCESS);
    }

    /// The first chunk commits; the second answers 502 and its re-check
    /// still reads new records. Some of the import is in place, so it exits 4.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_chunk_after_a_committed_one_exits_four() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let _env = CredentialsEnv::new(&server.uri());
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with({
                let writes = writes.clone();
                move |request: &wiremock::Request| match posted_chunk(request) {
                    (records, true) => seed_report(records, 0),
                    (records, false) => {
                        if writes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                            seed_report(records, 0)
                        } else {
                            ResponseTemplate::new(502).set_body_string("Bad Gateway")
                        }
                    }
                }
            })
            .mount(&server)
            .await;

        assert_eq!(import_people(&server, 80).await, ExitCode::from(4));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_import_that_committed_nothing_exits_two() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let _env = CredentialsEnv::new(&server.uri());
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with(|request: &wiremock::Request| match posted_chunk(request) {
                (records, true) => seed_report(records, 0),
                (_, false) => ResponseTemplate::new(502).set_body_string("Bad Gateway"),
            })
            .mount(&server)
            .await;

        assert_eq!(import_people(&server, 80).await, ExitCode::from(2));
    }

    /// A repository with no `seeds/` directory succeeds without a login or a
    /// network call — the same convention `project portal` uses for a
    /// repository with no application.
    #[tokio::test(flavor = "current_thread")]
    async fn seed_directory_succeeds_with_no_directory() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("seeds");

        assert_eq!(
            seed_directory(
                SeedCredential::Stored { host: None },
                &missing,
                SeedOptions::default(),
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    /// LAW-189: `--folder` names the matter folder on the pointer, which
    /// lands under `blobs/`.
    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_with_folder_records_it_on_the_blob() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "unclassified",
                "visibility": "internal",
                "current_version": {
                    "version": 1,
                    "asset_id": document_id,
                    "created_at": "2026-09-05T12:00:00Z",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 13
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("rogs.pdf");
        std::fs::write(&named, b"test document").unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();

        assert_eq!(
            document_upload_from(
                dir.path(),
                Some(server_uri.as_str()),
                "acme",
                &named,
                "unclassified",
                None,
                None,
                Some("application/pdf"),
                None,
                None,
                &DocumentExecution::default(),
                Some(store::document_pointers::MatterFolder::Discovery),
            )
            .await,
            ExitCode::SUCCESS
        );
        let raw = std::fs::read_to_string(dir.path().join("blobs/rogs.pdf.yaml")).unwrap();
        let pointer = store::document_pointers::DocumentPointer::from_yaml(&raw).unwrap();
        assert_eq!(
            pointer.folder,
            Some(store::document_pointers::MatterFolder::Discovery)
        );
        assert!(raw.contains("folder: Discovery\n"), "{raw}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_writes_a_pointer_when_run_from_its_project_checkout() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .and(body_json(serde_json::json!({
                "filename": "note.txt",
                "content_base64": "dGVzdCBkb2N1bWVudA==",
                "content_type": "text/plain",
                "kind": "unclassified",
                "visibility": "internal"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "unclassified",
                "visibility": "internal",
                "current_version": {
                    "version": 1,
                    "asset_id": document_id,
                    "created_at": "2026-09-05T12:00:00Z",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 13
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("note.txt");
        std::fs::write(&named, b"test document").unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();

        assert_eq!(
            document_upload_from(
                dir.path(),
                Some(server_uri.as_str()),
                "acme",
                &named,
                "unclassified",
                None,
                None,
                Some("text/plain"),
                None,
                None,
                &DocumentExecution::default(),
                None,
            )
            .await,
            ExitCode::SUCCESS
        );
        let pointer_path = dir.path().join("blobs/note.txt.yaml");
        let pointer = store::document_pointers::DocumentPointer::from_yaml(
            &std::fs::read_to_string(pointer_path).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer.current_version.asset_id, document_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_sends_the_execution_record_and_stages_it_on_the_pointer() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let signing = serde_json::json!({ "method": "docusign", "signed_on": "2026-09-04" });
        let notarization = serde_json::json!([{
            "method": "remote_online",
            "acknowledged_on": "2026-09-04",
            "state": "NV",
            "county": "Clark",
            "commission_expires": "2029-01-31",
        }]);

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .and(body_json(serde_json::json!({
                "filename": "note.txt",
                "content_base64": "dGVzdCBkb2N1bWVudA==",
                "content_type": "text/plain",
                "kind": "onboarding",
                "visibility": "internal",
                "signing": signing,
                "notarization": notarization,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "onboarding",
                "visibility": "internal",
                "current_version": {
                    "version": 1,
                    "asset_id": document_id,
                    "created_at": "2026-09-05T12:00:00Z",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 13
                },
                "signing": signing,
                "notarization": notarization,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("note.txt");
        std::fs::write(&named, b"test document").unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();

        let execution = DocumentExecution {
            signing: Some(store::documents::Signing {
                method: store::documents::SigningMethod::Docusign,
                signed_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
            }),
            notarization: vec![store::documents::Notarization {
                method: store::documents::NotarizationMethod::RemoteOnline,
                acknowledged_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
                state: "NV".into(),
                county: "Clark".into(),
                commission_expires: chrono::NaiveDate::from_ymd_opt(2029, 1, 31).unwrap(),
            }],
            evidence: None,
        };
        assert_eq!(
            document_upload_from(
                dir.path(),
                Some(server_uri.as_str()),
                "acme",
                &named,
                "onboarding",
                None,
                None,
                Some("text/plain"),
                None,
                None,
                &execution,
                None,
            )
            .await,
            ExitCode::SUCCESS
        );
        let pointer = store::document_pointers::DocumentPointer::from_yaml(
            &std::fs::read_to_string(dir.path().join("blobs/note.txt.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer.signing, execution.signing);
        assert_eq!(pointer.notarization, execution.notarization);
    }

    #[test]
    fn folder_slug_keeps_the_folders_below_documents_and_ignores_other_paths() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let slug = |file: &str| super::slug_from_documents_folder(root, &root.join(file));
        assert_eq!(
            slug("documents/onboarding/a_signed.pdf").as_deref(),
            Some("onboarding/a_signed.pdf")
        );
        assert_eq!(slug("documents/a/b/c.pdf").as_deref(), Some("a/b/c.pdf"));
        assert_eq!(
            slug("documents/documents/c.pdf").as_deref(),
            Some("documents/c.pdf")
        );
        // A matter folder is where bytes live, not part of the slug (LAW-189).
        assert_eq!(
            slug("documents/Discovery/set-one/c.pdf").as_deref(),
            Some("set-one/c.pdf")
        );
        assert_eq!(
            slug("documents/Work Product/c.pdf").as_deref(),
            Some("c.pdf")
        );
        // A fixed `Work Product` subfolder is matched whole, and its name is
        // not part of the slug (LAW-215); a lawyer-made subfolder inside one
        // stays in the slug.
        assert_eq!(
            slug("documents/Work Product/Research (Work Product)/memo.pdf").as_deref(),
            Some("memo.pdf")
        );
        assert_eq!(
            slug("documents/Work Product/Motions (Work Product)/motions/dismiss.pdf").as_deref(),
            Some("motions/dismiss.pdf")
        );
        // Files at the `documents/` root, or outside it, keep the filename slug.
        assert_eq!(slug("documents/c.pdf"), None);
        assert_eq!(slug("c.pdf"), None);
        assert_eq!(slug("scratch/onboarding/c.pdf"), None);
        // A relative path is read against the checkout it was typed in.
        assert_eq!(
            super::slug_from_documents_folder(root, std::path::Path::new("documents/x/y.pdf"))
                .as_deref(),
            Some("x/y.pdf")
        );
        assert_eq!(
            super::slug_from_documents_folder(root, std::path::Path::new("./documents/x/y.pdf"))
                .as_deref(),
            Some("x/y.pdf")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_slugs_a_file_under_documents_by_its_folder() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .and(body_json(serde_json::json!({
                "filename": "letter.txt",
                "content_base64": "dGVzdCBkb2N1bWVudA==",
                "content_type": "text/plain",
                "kind": "unclassified",
                "visibility": "internal",
                "slug": "onboarding/letter.txt"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "unclassified",
                "visibility": "internal",
                "current_version": {
                    "version": 1,
                    "asset_id": document_id,
                    "created_at": "2026-09-05T12:00:00Z",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 13
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/onboarding")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/onboarding")).unwrap();
        let named = dir.path().join("documents/onboarding/letter.txt");
        std::fs::write(&named, b"test document").unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:
  host: navigator.example.com
  name: acme
",
        )
        .unwrap();

        assert_eq!(
            document_upload_from(
                dir.path(),
                Some(server_uri.as_str()),
                "acme",
                &named,
                "unclassified",
                None,
                None,
                Some("text/plain"),
                None,
                None,
                &DocumentExecution::default(),
                None,
            )
            .await,
            ExitCode::SUCCESS
        );
        assert!(dir
            .path()
            .join("blobs/onboarding/letter.txt.yaml")
            .is_file());
        assert!(!dir.path().join("blobs/letter.txt.yaml").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_slug_patches_the_asset_and_dry_run_sends_dry_run() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}"
            )))
            .and(body_json(serde_json::json!({
                "slug": "notes/note.txt",
                "kind": "exhibit",
                "dry_run": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "asset_id": asset_id,
                "slug": "notes/note.txt",
                "kind": "exhibit",
                "dry_run": true,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            document_slug(
                Some(server_uri.as_str()),
                "acme",
                asset_id,
                "notes/note.txt",
                Some("exhibit"),
                true,
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn upload_visibility_notice_never_echoes_arbitrary_values() {
        for (requested, filed) in [
            ("synthetic-private-value", "internal"),
            ("client", "synthetic-private-value"),
        ] {
            let notice = super::visibility_mismatch_notice(Some(requested), filed, Uuid::nil())
                .expect("mismatched visibility is reported");
            assert!(
                !notice.contains("synthetic-private-value"),
                "visibility diagnostics must use closed labels"
            );
            assert!(notice.contains("unrecognized"));
        }
    }

    #[test]
    fn upload_visibility_notice_names_the_verb_when_the_filed_row_differs() {
        let notice = super::visibility_mismatch_notice(Some("client"), "internal", Uuid::nil())
            .expect("a differing visibility is reported");
        assert!(notice.contains("internal"), "{notice}");
        assert!(notice.contains("site document visibility"), "{notice}");
        assert!(notice.contains(&Uuid::nil().to_string()), "{notice}");
        assert_eq!(
            super::visibility_mismatch_notice(Some("client"), "client", Uuid::nil()),
            None
        );
        assert_eq!(
            super::visibility_mismatch_notice(None, "internal", Uuid::nil()),
            None
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_visibility_patches_only_the_visibility_of_the_asset() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}"
            )))
            .and(body_json(serde_json::json!({ "visibility": "client" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "changed": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            document_visibility(Some(server_uri.as_str()), "acme", asset_id, "client").await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_visibility_fails_when_the_row_is_not_found() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        assert_eq!(
            document_visibility(Some(server_uri.as_str()), "acme", asset_id, "client").await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_repair_posts_storage_with_dry_run() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}/storage"
            )))
            .and(body_json(serde_json::json!({ "dry_run": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "storage_key": "projects/acme/documents/abc",
                "copied": true,
                "dry_run": false,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            document_repair(Some(server_uri.as_str()), "acme", asset_id, false).await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_reclassify_posts_the_kind_to_its_own_door() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}/reclassify"
            )))
            .and(body_json(
                serde_json::json!({ "kind": "unclassified", "dry_run": true }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "asset_id": asset_id,
                "slug": "drafts/draft.md",
                "kind": "unclassified",
                "dry_run": true,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            document_reclassify(
                Some(server_uri.as_str()),
                "acme",
                asset_id,
                "unclassified",
                true
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_reclassify_fails_on_a_refusal() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "kind_unchanged",
                "message": "The document is already filed as `memo`.",
            })))
            .mount(&server)
            .await;

        assert_eq!(
            document_reclassify(Some(server_uri.as_str()), "acme", asset_id, "memo", false).await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_rejects_an_oversized_file_before_any_request() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("synthetic-document.bin");
        let file = std::fs::File::create(&named).unwrap();
        file.set_len((store::documents::MAX_DOCUMENT_UPLOAD_BYTES + 1) as u64)
            .unwrap();

        assert_eq!(
            document_upload(
                Some(server.uri().as_str()),
                "synthetic-project",
                &named,
                "unclassified",
                None,
                None,
                None,
                None,
                None,
                &DocumentExecution::default(),
                None,
            )
            .await,
            ExitCode::from(2)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mail_file_dry_run_lists_attachments_and_writes_nothing() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let message_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/mail/file")))
            .and(body_json(serde_json::json!({
                "message_id": message_id,
                "kind": "exhibit",
                "visibility": "internal",
                "dry_run": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message_id": message_id,
                "dry_run": true,
                "attachments": [
                    {"filename": "photo.png", "content_type": "image/png", "size_bytes": 42}
                ],
                "filed": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            mail_file(
                root.path(),
                Some(server_uri.as_str()),
                "acme",
                message_id,
                "exhibit",
                "internal",
                true,
            )
            .await,
            ExitCode::SUCCESS
        );
        assert!(
            !root.path().join("documents").exists(),
            "a dry run must write nothing"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mail_file_stages_one_pointer_per_filed_attachment() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let message_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/mail/file")))
            .and(body_json(serde_json::json!({
                "message_id": message_id,
                "kind": "exhibit",
                "visibility": "internal",
                "dry_run": false,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "message_id": message_id,
                "dry_run": false,
                "attachments": [
                    {"filename": "photo.png", "content_type": "image/png", "size_bytes": 42}
                ],
                "filed": [
                    {
                        "kind": "exhibit",
                        "visibility": "internal",
                        "current_version": {
                            "version": 1,
                            "asset_id": asset_id,
                            "created_at": "2026-09-06T00:00:00Z",
                            "sha256": "a".repeat(64),
                            "size_bytes": 42
                        }
                    }
                ],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            mail_file(
                root.path(),
                Some(server_uri.as_str()),
                "acme",
                message_id,
                "exhibit",
                "internal",
                false,
            )
            .await,
            ExitCode::SUCCESS
        );
        let staged = root
            .path()
            .join("blobs/mail")
            .join(message_id.to_string())
            .join("0.yml");
        assert!(staged.is_file(), "{}", staged.display());
        let pointer = store::document_pointers::DocumentPointer::from_yaml(
            &std::fs::read_to_string(&staged).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer.current_version.asset_id, asset_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mail_file_with_no_attachments_writes_nothing_and_exits_cleanly() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let message_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/mail/file")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message_id": message_id,
                "dry_run": false,
                "attachments": [],
                "filed": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            mail_file(
                root.path(),
                Some(server_uri.as_str()),
                "acme",
                message_id,
                "exhibit",
                "internal",
                false,
            )
            .await,
            ExitCode::SUCCESS
        );
        assert!(!root.path().join("documents").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_refuses_an_unknown_matter_code() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::now_v7(), "code": "acme"}
            ])))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("note.txt");
        std::fs::write(&named, b"test document").unwrap();

        assert_eq!(
            document_upload(
                Some(server_uri.as_str()),
                "not-a-matter",
                &named,
                "unclassified",
                None,
                None,
                None,
                None,
                None,
                &DocumentExecution::default(),
                None,
            )
            .await,
            ExitCode::from(2)
        );
    }

    /// `git init`, one committed file, in a fresh temp dir. Returns the
    /// directory and the resulting commit SHA, so a test can assert the
    /// archive command reads back exactly what it just committed.
    fn git_fixture() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("README.md"), b"hello").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "--quiet", "-m", "initial"]);
        let sha = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse HEAD");
        let sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();
        (dir, sha)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn archive_repository_records_the_commit_sha_and_uploads_a_zip() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let (repo, commit_sha) = git_fixture();
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "closed_repository",
                "visibility": "internal",
                "current_version": {
                    "version": 1,
                    "asset_id": document_id,
                    "created_at": "2026-09-05T12:00:00Z",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 300
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            archive_repository(Some(server_uri.as_str()), "acme", repo.path()).await,
            ExitCode::SUCCESS
        );

        // The exact body the mock above accepted (any body) is asserted more
        // precisely below via the request log, so the commit sha this test
        // fixture produced is provably what was sent, not merely a mock that
        // would have matched anything.
        let requests = server.received_requests().await.unwrap();
        let upload = requests
            .iter()
            .find(|r| r.url.path().ends_with("/documents"))
            .expect("the upload request was made");
        let body: serde_json::Value = serde_json::from_slice(&upload.body).unwrap();
        assert_eq!(body["kind"], "closed_repository");
        assert_eq!(body["slug"], "closed_repository.zip");
        assert_eq!(body["metadata"]["commit_sha"], commit_sha);
        assert_eq!(body["content_type"], "application/zip");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_bytes_refuses_an_oversized_document_before_sending_it() {
        // `archive_repository` has no file to stat — it builds a repository
        // snapshot in memory and hands it straight to `upload_bytes`, so the
        // limit has to hold at the shared choke point and not only at the
        // one command that can check a path's metadata first.
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;

        let client = DocumentClient::connect(Some(server_uri.as_str()), "acme")
            .await
            .expect("connect to the mock door");
        // The per-kind caps route sizes to a lane; the streaming ceiling is
        // the one size no lane accepts, and it is refused before anything
        // hits the wire.
        let oversized = vec![0u8; store::documents::MAX_STREAMING_UPLOAD_BYTES + 1];
        let error = client
            .upload_bytes(
                "synthetic-repository.zip",
                &oversized,
                "closed_repository",
                None,
                None,
                Some("application/zip"),
                None,
                None,
            )
            .await
            .expect_err("an upload above the streaming ceiling is refused before the request");

        assert!(error
            .to_string()
            .contains(&store::documents::MAX_STREAMING_UPLOAD_BYTES.to_string()));
        assert!(error.to_string().contains(&oversized.len().to_string()));
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| {
                !request.url.path().ends_with("/documents")
                    && !request.url.path().contains("/upload-session")
            }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_rejects_transcript_quality_for_other_asset_kinds() {
        assert_eq!(
            document_upload(
                None,
                "acme",
                std::path::Path::new("missing.pdf"),
                "filing",
                None,
                None,
                None,
                None,
                Some("machine"),
                &DocumentExecution::default(),
                None,
            )
            .await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_transcribe_rejects_a_non_pdf_pointer_before_connecting() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("navigator.yaml"),
            "project:\n  name: acme\n  host: https://navigator.example.com\n",
        )
        .unwrap();
        let pointer_path = root.path().join("blobs/cases/order.docx.yml");
        std::fs::create_dir_all(pointer_path.parent().unwrap()).unwrap();
        let pointer = store::document_pointers::DocumentPointer {
            checkout_path: None,
            kind: "filing".into(),
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
            xero_invoice_id: None,
            signing: None,
            notarization: Vec::new(),
            service: Vec::new(),
            evidence: None,
        };
        std::fs::write(&pointer_path, pointer.to_yaml().unwrap()).unwrap();

        assert_eq!(
            document_transcribe_from(root.path(), &pointer_path).await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_transcribe_rejects_missing_project_inputs() {
        let root = tempfile::tempdir().unwrap();
        let pointer = std::path::Path::new("blobs/cases/order.pdf.yml");
        assert_eq!(
            document_transcribe_from(root.path(), pointer).await,
            ExitCode::from(2)
        );

        std::fs::write(
            root.path().join("navigator.yaml"),
            "project:\n  name: acme\n",
        )
        .unwrap();
        assert_eq!(
            document_transcribe_from(root.path(), pointer).await,
            ExitCode::from(2)
        );

        std::fs::write(
            root.path().join("navigator.yaml"),
            "host: https://navigator.example.com\n",
        )
        .unwrap();
        assert_eq!(
            document_transcribe_from(root.path(), pointer).await,
            ExitCode::from(2)
        );

        std::fs::write(
            root.path().join("navigator.yaml"),
            "project:\n  name: acme\n  host: https://navigator.example.com\n",
        )
        .unwrap();
        assert_eq!(
            document_transcribe_from(root.path(), pointer).await,
            ExitCode::from(2)
        );
    }

    #[test]
    fn transcript_source_revision_must_match_the_operative_pointer() {
        let asset_id = Uuid::now_v7();
        let pointer = store::document_pointers::PointerVersion {
            version: 1,
            asset_id,
            created_at: "2026-09-05T12:00:00Z".into(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            canonical_url: None,
            checked_on: None,
        };
        let revision = RevisionSummary {
            version: 1,
            asset_id,
            created_at: "2026-09-05T12:00:00Z".into(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            filename: "order.pdf".into(),
            visibility: "internal".into(),
            operative: true,
            deleted_at: None,
            signing: None,
            notarization: Vec::new(),
        };

        assert!(source_revision(&pointer, &[])
            .unwrap_err()
            .to_string()
            .contains("no longer on the live document chain"));
        assert!(source_revision(&pointer, std::slice::from_ref(&revision)).is_ok());

        let mut stale = revision.clone();
        stale.operative = false;
        assert!(source_revision(&pointer, std::slice::from_ref(&stale)).is_err());
        stale.operative = true;
        stale.version = 2;
        assert!(source_revision(&pointer, std::slice::from_ref(&stale)).is_err());
        stale.version = 1;
        stale.sha256 = "b".repeat(64);
        assert!(source_revision(&pointer, &[stale]).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_bytes_refuses_a_slug_without_the_filename_extension() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;

        let client = DocumentClient::connect(Some(server_uri.as_str()), "acme")
            .await
            .expect("connect to the mock door");
        let error = client
            .upload_bytes(
                "complaint.pdf",
                b"synthetic document",
                "unclassified",
                None,
                None,
                Some("application/pdf"),
                Some("dkt-001-complaint"),
                None,
            )
            .await
            .expect_err("a bare slug cannot become a Pointer");

        assert_eq!(
            error.to_string(),
            "document slug `dkt-001-complaint` must carry the source extension from `complaint.pdf`; use `dkt-001-complaint.pdf` so it can be represented as a Pointer"
        );
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| !request.url.path().ends_with("/documents")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn archive_repository_refuses_a_directory_with_no_git_head() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let empty = tempfile::tempdir().unwrap();

        assert_eq!(
            archive_repository(Some(server_uri.as_str()), "acme", empty.path()).await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matter_close_refuses_an_unknown_matter_code() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::now_v7(), "code": "acme"}
            ])))
            .mount(&server)
            .await;

        assert_eq!(
            matter_close(
                Some(server_uri.as_str()),
                "not-a-matter",
                store::projects::ClosureReason::PitchDeclined,
                None
            )
            .await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matter_close_reports_a_server_refusal() {
        // An archived matter refuses every transition but a repeated
        // archive — the CLI must surface the server's 400, not claim success.
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/lifecycle")))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_request",
                "message": "This matter is archived; archiving is terminal.",
            })))
            .mount(&server)
            .await;

        assert_eq!(
            matter_close(
                Some(server_uri.as_str()),
                "acme",
                store::projects::ClosureReason::EngagementCompleted,
                None
            )
            .await,
            ExitCode::from(2)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matter_close_posts_the_effective_time_to_the_lifecycle_api() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        let effective_at: chrono::DateTime<chrono::Utc> = "2001-02-03T04:05:06Z".parse().unwrap();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/lifecycle")))
            .and(body_json(serde_json::json!({
                "transition": "close",
                "reason": "engagement_completed",
                "effective_at": "2001-02-03T04:05:06Z"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": project_id,
                "code": "acme",
                "status": "closed",
                "closed_at": "2001-02-03T04:05:06Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            matter_close(
                Some(server_uri.as_str()),
                "acme",
                store::projects::ClosureReason::EngagementCompleted,
                Some(effective_at)
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scaffold_duplicate_code_preflight_is_read_only() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::now_v7(), "code": "matter-one"}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        assert!(project_code_exists(Some(&server_uri), "matter-one")
            .await
            .unwrap());
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == wiremock::http::Method::GET));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unknown_jurisdiction_is_refused_before_creating_a_contact() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/entity-types"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id":Uuid::now_v7(),"name":"Human"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/jurisdictions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        assert_eq!(
            projects_create_named(
                Some(&server_uri),
                "Matter",
                "matter-one",
                "New Client",
                "client@example.com",
                None,
                Some("Mars"),
                true,
                None,
                None,
                None,
            )
            .await,
            ExitCode::from(2)
        );
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == wiremock::http::Method::GET));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scaffold_creates_missing_client_and_reuses_the_existing_entity() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let client_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let project_id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/people"))
            .and(body_json(serde_json::json!({
                "name":"New Client", "email":"new-client@example.com", "role":"client"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id":client_id,"email":"new-client@example.com","name":"New Client","role":"client"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/entities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id":entity_id,"name":"Acme LLC"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects"))
            .and(body_json(serde_json::json!({
                "name":"Acme Formation","code":"acme-formation","client_id":client_id,
                "entity_id":entity_id,"attestation":true
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id":project_id,"name":"Acme Formation","code":"acme-formation",
                "status":"open","entity_id":entity_id
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            projects_create_named(
                Some(&server_uri),
                "Acme Formation",
                "acme-formation",
                "New Client",
                "new-client@example.com",
                Some("Acme LLC"),
                None,
                true,
                None,
                None,
                None,
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projects_create_resolves_client_and_existing_entity_then_opens() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let client_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let project_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": client_id, "email": "client@example.com", "name": "Acme Client", "role": "client"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/entities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": entity_id, "name": "Acme LLC"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects"))
            .and(body_json(serde_json::json!({
                "name": "Acme Formation",
                "code": "acme-formation",
                "client_id": client_id,
                "entity_id": entity_id,
                "attestation": true,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": project_id,
                "name": "Acme Formation",
                "code": "acme-formation",
                "status": "open",
                "entity_id": entity_id,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            projects_create(
                Some(&server_uri),
                "Acme Formation",
                "acme-formation",
                "client@example.com",
                Some("Acme LLC"),
                None,
                true,
                None,
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projects_create_sends_the_selected_firm_and_brand() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let client_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let project_id = Uuid::now_v7();
        let firm_id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/firms"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": firm_id, "name":"Example Practice", "brands":["sample"]}
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": client_id, "email": "client@example.com", "name": "Acme Client", "role": "client"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/entities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": entity_id, "name": "Acme LLC"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects"))
            .and(body_json(serde_json::json!({
                "name": "Acme Formation",
                "code": "acme-formation",
                "client_id": client_id,
                "entity_id": entity_id,
                "attestation": true,
                "firm_id": firm_id,
                "brand": "sample",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": project_id,
                "name": "Acme Formation",
                "code": "acme-formation",
                "status": "open",
                "entity_id": entity_id,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            super::projects_create_named(
                Some(&server_uri),
                "Acme Formation",
                "acme-formation",
                "",
                "client@example.com",
                Some("Acme LLC"),
                None,
                true,
                None,
                Some("Example Practice"),
                Some("sample"),
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projects_create_without_entity_name_creates_a_human_entity_in_the_jurisdiction() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let client_id = Uuid::now_v7();
        let entity_type_id = Uuid::now_v7();
        let jurisdiction_id = Uuid::now_v7();
        let created_entity_id = Uuid::now_v7();
        let project_id = Uuid::now_v7();

        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": client_id, "email": "solo@example.com", "name": "Solo Client", "role": "client"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/entity-types"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": entity_type_id, "name": "Human"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/jurisdictions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": jurisdiction_id, "name": "California", "code": "CA"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/entities"))
            .and(body_json(serde_json::json!({
                "name": "Solo Client",
                "entity_type_id": entity_type_id,
                "jurisdiction_id": jurisdiction_id,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": created_entity_id,
                "name": "Solo Client",
                "entity_type_id": entity_type_id,
                "jurisdiction_id": jurisdiction_id,
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects"))
            .and(body_json(serde_json::json!({
                "name": "Solo Matter",
                "code": "solo-matter",
                "client_id": client_id,
                "entity_id": created_entity_id,
                "attestation": true,
                "jurisdiction_id": jurisdiction_id,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": project_id,
                "name": "Solo Matter",
                "code": "solo-matter",
                "status": "open",
                "entity_id": created_entity_id,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            projects_create(
                Some(&server_uri),
                "Solo Matter",
                "solo-matter",
                "solo@example.com",
                None,
                Some("CA"),
                true,
                None,
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    /// ENG-469: `--closed`/`--closed-at` reach the server as `status`/
    /// `closed_at` on the same open call.
    #[tokio::test(flavor = "current_thread")]
    async fn projects_create_opens_already_closed() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let client_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let project_id = Uuid::now_v7();
        let closed_at: chrono::DateTime<chrono::Utc> = "2026-06-01T00:00:00Z".parse().unwrap();

        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": client_id, "email": "client@example.com", "name": "Acme Client", "role": "client"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/entities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": entity_id, "name": "Acme LLC"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects"))
            .and(body_json(serde_json::json!({
                "name": "Already Closed",
                "code": "already-closed",
                "client_id": client_id,
                "entity_id": entity_id,
                "attestation": true,
                "status": "closed",
                "closed_at": "2026-06-01T00:00:00Z",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": project_id,
                "name": "Already Closed",
                "code": "already-closed",
                "status": "closed",
                "entity_id": entity_id,
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            projects_create(
                Some(&server_uri),
                "Already Closed",
                "already-closed",
                "client@example.com",
                Some("Acme LLC"),
                None,
                true,
                Some(closed_at),
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projects_create_refuses_a_non_client_email() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);

        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::now_v7(), "email": "lawyer@example.com", "name": "A Lawyer", "role": "lawyer"}
            ])))
            .mount(&server)
            .await;

        assert_eq!(
            projects_create(
                Some(&server_uri),
                "Bad Client",
                "bad-client",
                "lawyer@example.com",
                Some("Acme LLC"),
                None,
                true,
                None,
            )
            .await,
            ExitCode::from(2)
        );
    }

    /// Omitting both `--entity-name` and `--jurisdiction` is refused before
    /// any entity-resolving HTTP call — there is nothing to resolve a
    /// `Human` entity's jurisdiction from otherwise.
    #[tokio::test(flavor = "current_thread")]
    async fn projects_create_without_entity_name_or_jurisdiction_is_refused() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);

        Mock::given(method("GET"))
            .and(path("/app/api/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::now_v7(), "email": "client@example.com", "name": "A Client", "role": "client"}
            ])))
            .mount(&server)
            .await;
        // No `/app/api/entity-types` mock: a call there would 404 and the
        // refusal would read as a server error rather than a caller error.

        assert_eq!(
            projects_create(
                Some(&server_uri),
                "No Entity",
                "no-entity",
                "client@example.com",
                None,
                None,
                true,
                None,
            )
            .await,
            ExitCode::from(2)
        );
    }

    async fn mount_project_routes(server: &MockServer, ids: LawyerRouteIds) {
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": ids.project, "code": "acme"}
            ])))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{}/lifecycle", ids.project)))
            .and(body_json(
                serde_json::json!({ "transition": "close", "reason": "engagement_completed" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": ids.project,
                "code": "acme",
                "name": "Acme",
                "status": "closed"
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_notation_routes(server: &MockServer, ids: LawyerRouteIds) {
        Mock::given(method("POST"))
            .and(path("/app/projects/acme/notations/new"))
            .respond_with(ResponseTemplate::new(303).append_header(
                "location",
                format!("/app/lawyer/notations/{}/step", ids.notation),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{}/step", ids.notation)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "question": {"code": "q1", "prompt": "Question one", "answer_type": "text"}
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/lawyer/notations/{}/approve-send",
                ids.notation
            )))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/lawyer/notations/{}/documents/document",
                ids.notation
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pdf bytes".to_vec()))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/lawyer/notations/{}/send", ids.notation)))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/lawyer/notations/{}/request-changes",
                ids.notation
            )))
            .respond_with(ResponseTemplate::new(303).append_header(
                "location",
                format!("/app/lawyer/notations/{}/reask", ids.notation),
            ))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/lawyer/notations/{}/reask",
                ids.notation
            )))
            .respond_with(ResponseTemplate::new(303).append_header(
                "location",
                format!("/app/lawyer/notations/{}/review", ids.notation),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/lawyer/notations/{}/review",
                ids.notation
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "state": "lawyer_review",
                "delivery": "embedded",
                "document_ready": true
            })))
            .mount(server)
            .await;
    }

    async fn mount_notation_read_routes(server: &MockServer, ids: LawyerRouteIds) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{}/notation-inventory",
                ids.project
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/notations/{}/answers", ids.notation)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(server)
            .await;
    }

    async fn exercise_project_commands(host: Option<&str>) {
        assert_eq!(
            matter_close(
                host,
                "acme",
                store::projects::ClosureReason::EngagementCompleted,
                None
            )
            .await,
            ExitCode::SUCCESS
        );
    }

    async fn exercise_notation_commands(host: Option<&str>, server_uri: &str, ids: LawyerRouteIds) {
        assert_eq!(notation_list(host, "acme", true).await, ExitCode::SUCCESS);
        assert_eq!(
            notation_create(
                host,
                "memo__contract_review",
                "libra@example.com",
                "acme",
                None
            )
            .await,
            ExitCode::SUCCESS
        );
        let client = reqwest::Client::new();
        assert!(fetch_step(&client, server_uri, "test-token", ids.notation)
            .await
            .unwrap()
            .question
            .is_some());
        assert_eq!(
            notation_approve(host, ids.notation).await,
            ExitCode::SUCCESS
        );
        assert_eq!(
            notation_answers(host, ids.notation, true, None).await,
            ExitCode::SUCCESS
        );
        let out = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            notation_document(host, ids.notation, out.path()).await,
            ExitCode::SUCCESS
        );
        assert_eq!(std::fs::read(out.path()).unwrap(), b"pdf bytes");
        assert_eq!(
            notation_status(host, ids.notation, true).await,
            ExitCode::SUCCESS
        );
        assert_eq!(
            notation_request_changes(
                host,
                ids.notation,
                &["person__client".to_string()],
                Some("confirm the spelling"),
            )
            .await,
            ExitCode::SUCCESS
        );
        assert_eq!(
            notation_update(
                host,
                ids.notation,
                &["person__client=Libra Jones".to_string()],
            )
            .await,
            ExitCode::SUCCESS
        );
        assert_eq!(
            fetch_status(server_uri, "test-token", ids.notation)
                .await
                .unwrap()
                .state,
            "lawyer_review"
        );
    }

    /// The answer round trip: save a notation's filed answers with
    /// `--save-answers`, then prefill a second notation from that file with
    /// `--answers-from` and assert the second notation's walk POSTs
    /// exactly the saved values — a notation's interpolated fields are a
    /// pure function of its filed answers, so the two notations render the
    /// same fields.
    #[tokio::test(flavor = "current_thread")]
    async fn saved_answers_prefill_a_second_notation_with_the_same_fields() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let uri = server.uri();
        let host = Some(uri.as_str());
        let _env = CredentialsEnv::new(&uri);
        let filed = Uuid::new_v4();
        let second = Uuid::new_v4();

        // The first notation's filed answers, as the answers door returns them.
        Mock::given(method("GET"))
            .and(path(format!("/app/api/notations/{filed}/answers")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": Uuid::new_v4(),
                    "question_id": Uuid::new_v4(),
                    "state_name": "client_full_name",
                    "value": "Libra Example",
                    "source": "client",
                    "authored_by_person_id": null,
                },
                {
                    "id": Uuid::new_v4(),
                    "question_id": Uuid::new_v4(),
                    "state_name": "matter_description",
                    "value": "Estate plan for Example Co",
                    "source": "client",
                    "authored_by_person_id": null,
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        // Save half: persist the filed answers to disk.
        let dir = tempfile::tempdir().unwrap();
        let answers_path = dir.path().join("answers.yaml");
        assert_eq!(
            notation_answers(host, filed, false, Some(&answers_path)).await,
            ExitCode::SUCCESS
        );
        let saved = crate::answers_file::load_answers_file(&answers_path).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(
            crate::answers_file::saved_scalar(&saved["client_full_name"]).unwrap(),
            "Libra Example"
        );
        assert_eq!(
            crate::answers_file::saved_scalar(&saved["matter_description"]).unwrap(),
            "Estate plan for Example Co"
        );

        // Opening the second notation: the create 303s to the walker.
        Mock::given(method("POST"))
            .and(path("/app/projects/acme/notations/new"))
            .respond_with(
                ResponseTemplate::new(303)
                    .append_header("location", format!("/app/lawyer/notations/{second}/step")),
            )
            .mount(&server)
            .await;

        // The prefill walk: the first step asks one text question the
        // answers file covers; every step after that is the completed page.
        // wiremock serves the first matching mock in registration order, so
        // the one-shot question mock must be mounted before the completed
        // fallback.
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{second}/step")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "question": {
                    "code": "client_full_name",
                    "prompt": "What is the client's full name?",
                    "answer_type": "text",
                }
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{second}/step")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/lawyer/notations/{second}/step")))
            .respond_with(ResponseTemplate::new(303))
            .expect(1)
            .mount(&server)
            .await;

        // Prefill half: open the second notation from the saved file.
        assert_eq!(
            notation_create(
                host,
                "onboarding__letter",
                "libra@example.com",
                "acme",
                Some(&answers_path),
            )
            .await,
            ExitCode::SUCCESS
        );

        // The walk POSTed exactly the saved value as the answer field.
        let posted: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| {
                request.method == "POST"
                    && request.url.path() == format!("/app/lawyer/notations/{second}/step")
            })
            .map(|request| String::from_utf8(request.body.clone()).unwrap())
            .collect();
        assert_eq!(posted, vec!["value=Libra+Example".to_string()]);
    }

    /// A malformed answers file stops the prefill with a failure exit — a
    /// partial prefill would leave the notation half-answered while looking
    /// complete.
    #[tokio::test(flavor = "current_thread")]
    async fn a_malformed_answers_file_fails_the_prefill_before_answering_anything() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let uri = server.uri();
        let host = Some(uri.as_str());
        let _env = CredentialsEnv::new(&uri);
        let notation = Uuid::new_v4();

        Mock::given(method("POST"))
            .and(path("/app/projects/acme/notations/new"))
            .respond_with(
                ResponseTemplate::new(303)
                    .append_header("location", format!("/app/lawyer/notations/{notation}/step")),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let malformed = dir.path().join("answers.yaml");
        std::fs::write(&malformed, "just a string, not a map\n").unwrap();

        assert_ne!(
            notation_create(
                host,
                "onboarding__letter",
                "libra@example.com",
                "acme",
                Some(&malformed),
            )
            .await,
            ExitCode::SUCCESS
        );
        // Nothing was answered: the file failed before the walk began.
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method != "POST"
                || request.url.path() != format!("/app/lawyer/notations/{notation}/step")));
    }

    impl Drop for CredentialsEnv {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", previous);
            } else {
                std::env::remove_var("NAVIGATOR_CREDENTIALS_FILE");
            }
        }
    }

    #[test]
    fn step_json_deserializes_picker_candidates() {
        let mexico = Uuid::now_v7();
        let france = Uuid::now_v7();
        let step: StepResponse = serde_json::from_value(serde_json::json!({
            "question": {
                "code": "country__of_birth",
                "prompt": "Where were you born?",
                "answer_type": "country",
                "choices": [],
                "candidates": [
                    {"id": mexico, "name": "Mexico"},
                    {"id": france, "name": "France"}
                ]
            }
        }))
        .unwrap();
        let question = step.question.unwrap();
        assert_eq!(question.candidates.len(), 2);
        assert_eq!(question.candidates[0].id, mexico);
    }

    #[test]
    fn picker_selection_accepts_list_number_or_id() {
        let mexico = Uuid::now_v7();
        let france = Uuid::now_v7();
        let step: StepResponse = serde_json::from_value(serde_json::json!({
            "question": {
                "code": "country__of_birth",
                "prompt": "Where were you born?",
                "answer_type": "country",
                "candidates": [
                    {"id": mexico, "name": "Mexico"},
                    {"id": france, "name": "France"}
                ]
            }
        }))
        .unwrap();
        let question = step.question.unwrap();

        assert_eq!(select_candidate(&question, "2").unwrap().id, france);
        assert_eq!(
            select_candidate(&question, &mexico.to_string()).unwrap().id,
            mexico
        );
        assert!(select_candidate(&question, "3").is_err());
        assert!(select_candidate(&question, "France").is_err());
    }

    #[test]
    fn picker_selection_posts_the_selected_id_field() {
        let picked = Uuid::now_v7();
        let step: StepResponse = serde_json::from_value(serde_json::json!({
            "question": {
                "code": "entity__company",
                "prompt": "Which entity?",
                "answer_type": "entity",
                "candidates": [
                    {"id": picked, "name": "Bright Star Ventures LLC"}
                ]
            }
        }))
        .unwrap();
        let question = step.question.unwrap();

        assert_eq!(
            picker_selection_fields(&question, "1").unwrap(),
            vec![("id".to_string(), picked.to_string())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_commands_call_lawyer_routes() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let host = Some(server_uri.as_str());
        let ids = LawyerRouteIds {
            notation: Uuid::now_v7(),
            project: Uuid::now_v7(),
        };
        mount_project_routes(&server, ids).await;
        mount_notation_routes(&server, ids).await;
        mount_notation_read_routes(&server, ids).await;

        exercise_project_commands(host).await;
        exercise_notation_commands(host, &server_uri, ids).await;
    }

    /// The council's "no opaque 500" rule lives in `server_error`: when a
    /// route answers with the `{error, reason}` shape, both halves reach the
    /// terminal, because the reason is the part that tells someone what to do
    /// next.
    #[test]
    fn a_json_error_body_renders_the_error_and_its_reason() {
        assert_eq!(
            server_error(
                reqwest::StatusCode::CONFLICT,
                r#"{"error":"already approved","reason":"the notation left review"}"#,
            ),
            "409 Conflict: already approved — the notation left review"
        );
    }

    /// Either field alone still beats printing the raw body.
    #[test]
    fn a_json_error_body_carrying_one_field_renders_that_field() {
        assert_eq!(
            server_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"reason":"you do not participate on this matter"}"#,
            ),
            "403 Forbidden: you do not participate on this matter"
        );
        assert_eq!(
            server_error(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"unknown step"}"#
            ),
            "400 Bad Request: unknown step"
        );
    }

    /// A body that parses as JSON but carries neither field is not a
    /// recognized error shape, so it takes the plain-text path rather than
    /// silently reporting the status with no detail at all.
    #[test]
    fn a_json_body_with_neither_field_falls_back_to_the_body_text() {
        assert_eq!(
            server_error(reqwest::StatusCode::NOT_FOUND, r#"{"detail":"nope"}"#),
            r#"404 Not Found: {"detail":"nope"}"#
        );
    }

    /// An HTML error page must not reach the terminal whole — a proxy 502
    /// answers with a document, not a sentence.
    #[test]
    fn a_non_json_error_body_is_reduced_to_its_first_real_line() {
        assert_eq!(
            server_error(
                reqwest::StatusCode::BAD_GATEWAY,
                "\n\n  <html><head><title>502 Bad Gateway</title></head>\n<body>more markup</body>\n",
            ),
            "502 Bad Gateway: <html><head><title>502 Bad Gateway</title></head>"
        );
    }

    /// LAW-157: a request the CLI abandons is named as a timeout with its
    /// limit, since reqwest's own message does not say so.
    #[tokio::test]
    async fn an_abandoned_integrity_request_is_named_as_a_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(2)))
            .mount(&server)
            .await;
        let limit = std::time::Duration::from_millis(50);
        let error = reqwest::Client::new()
            .get(server.uri())
            .timeout(limit)
            .send()
            .await
            .unwrap_err();
        assert!(error.is_timeout());

        let failure = integrity_failure(Err(&error), limit);

        assert!(failure.contains("timed out after 0s"), "{failure}");
    }

    #[test]
    fn a_gateway_html_body_is_not_echoed_in_an_integrity_failure() {
        let limit = std::time::Duration::from_secs(100);
        for status in [
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::GATEWAY_TIMEOUT,
        ] {
            let failure = integrity_failure(Ok((status, "<html><head><title>x</title>")), limit);
            assert!(failure.starts_with(&status.to_string()), "{failure}");
            assert!(failure.contains("timed out"), "{failure}");
            assert!(!failure.contains('<'), "{failure}");
        }
        assert_eq!(
            integrity_failure(
                Ok((reqwest::StatusCode::BAD_GATEWAY, "upstream connect error")),
                limit
            ),
            "502 Bad Gateway: upstream connect error"
        );
        assert_eq!(
            integrity_failure(Ok((reqwest::StatusCode::NOT_FOUND, "not found")), limit),
            "404 Not Found: not found"
        );
    }

    #[test]
    fn trimmed_body_drops_markup_and_collapses_whitespace() {
        assert_eq!(
            trimmed_body(
                "<html><head><title>502 Bad Gateway</title></head>\n<body>\n  upstream  down\n</body></html>"
            ),
            "502 Bad Gateway upstream down"
        );
        assert_eq!(trimmed_body("  \n"), "(empty response)");
        assert_eq!(trimmed_body(&"x".repeat(500)).len(), 200);
    }

    async fn lookup(server: &MockServer) -> anyhow::Result<String> {
        fetch_project_list_with(
            &reqwest::Client::new(),
            &server.uri(),
            "t",
            3,
            std::time::Duration::ZERO,
        )
        .await
    }

    #[tokio::test]
    async fn project_lookup_retries_502_and_503_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html><head>"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;
        assert_eq!(lookup(&server).await.expect("third try succeeds"), "[]");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn project_lookup_gives_up_at_the_cap_with_status_and_trimmed_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(502)
                    .set_body_string("<html><body><h1>502 Bad Gateway</h1></body></html>"),
            )
            .mount(&server)
            .await;
        let error = format!("{:#}", lookup(&server).await.unwrap_err());
        assert!(
            error.contains("502 Bad Gateway: 502 Bad Gateway"),
            "{error}"
        );
        assert!(error.contains("attempt 3 of 3"), "{error}");
        assert!(!error.contains('<'), "{error}");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn project_lookup_does_not_retry_401_or_404() {
        for status in [401u16, 404] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/app/api/projects"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let error = lookup(&server).await.unwrap_err();
            if status == 401 {
                assert!(error.downcast_ref::<UploadSessionExpired>().is_some());
            } else {
                assert!(format!("{error:#}").contains("404"));
            }
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[test]
    fn first_line_skips_leading_blank_lines_and_trims() {
        assert_eq!(
            first_line("\n\n   the real line  \nsecond\n"),
            "the real line"
        );
    }

    /// A body with nothing printable in it is named rather than rendered as
    /// an empty string, so the error does not read as a blank.
    #[test]
    fn an_empty_body_is_named() {
        assert_eq!(first_line(""), "(empty response)");
        assert_eq!(first_line("\n   \n\t\n"), "(empty response)");
    }

    /// The 200-character cap is what stops a whole minified page from
    /// scrolling the terminal, and it counts characters rather than bytes so
    /// a multi-byte body cannot be cut mid-character.
    #[test]
    fn a_long_first_line_is_capped_at_two_hundred_characters() {
        assert_eq!(first_line(&"x".repeat(500)).chars().count(), 200);

        let wide = first_line(&"é".repeat(500));
        assert_eq!(wide.chars().count(), 200);
        assert_eq!(wide, "é".repeat(200));
    }

    #[test]
    fn a_local_pointer_follows_a_moved_slug_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("blobs/notes/a.pdf.yaml");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "pointer").unwrap();

        let moved = move_local_pointer(dir.path(), "notes/a.pdf", "filed/forms/a.pdf").unwrap();
        assert_eq!(
            moved.as_ref().map(|moved| moved.pointer.as_str()),
            Some("blobs/filed/forms/a.pdf.yaml")
        );
        assert_eq!(moved.and_then(|moved| moved.bytes), None);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("blobs/filed/forms/a.pdf.yaml")).unwrap(),
            "pointer"
        );
        assert_eq!(
            move_local_pointer(dir.path(), "notes/a.pdf", "x/a.pdf").unwrap(),
            None,
            "no pointer for the previous slug is not an error"
        );

        std::fs::create_dir_all(dir.path().join("documents/pre-filed/forms")).unwrap();

        std::fs::create_dir_all(dir.path().join("blobs/pre-filed/forms")).unwrap();
        std::fs::write(dir.path().join("blobs/pre-filed/forms/a.pdf.yaml"), "other").unwrap();
        assert!(
            move_local_pointer(dir.path(), "filed/forms/a.pdf", "pre-filed/forms/a.pdf").is_err()
        );
        assert!(dir.path().join("blobs/filed/forms/a.pdf.yaml").exists());
    }

    #[test]
    fn a_legacy_bare_slug_pointer_is_found_by_its_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs")).unwrap();
        std::fs::write(dir.path().join("blobs/summons.pdf.yaml"), "pointer").unwrap();
        let moved = move_local_pointer(dir.path(), "summons", "filed/summons.pdf").unwrap();
        assert_eq!(
            moved.as_ref().map(|moved| moved.pointer.as_str()),
            Some("blobs/filed/summons.pdf.yaml")
        );
    }

    #[test]
    fn a_local_pointer_and_its_bytes_move_together() {
        let dir = tempfile::tempdir().unwrap();
        let old_pointer = dir.path().join("blobs/notes/a.pdf.yaml");
        let old_bytes = dir.path().join("documents/notes/a.pdf");
        std::fs::create_dir_all(old_pointer.parent().unwrap()).unwrap();
        std::fs::create_dir_all(old_bytes.parent().unwrap()).unwrap();
        std::fs::write(&old_pointer, "pointer").unwrap();
        std::fs::write(&old_bytes, "pdf-bytes").unwrap();

        let moved = move_local_pointer(dir.path(), "notes/a.pdf", "filed/forms/a.pdf").unwrap();
        let moved = moved.expect("the pointer and bytes both move");
        assert_eq!(moved.pointer, "blobs/filed/forms/a.pdf.yaml");
        assert_eq!(moved.bytes.as_deref(), Some("documents/filed/forms/a.pdf"));
        assert!(!old_pointer.exists());
        assert!(!old_bytes.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("blobs/filed/forms/a.pdf.yaml")).unwrap(),
            "pointer"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("documents/filed/forms/a.pdf")).unwrap(),
            "pdf-bytes"
        );
    }

    #[test]
    fn a_move_refuses_target_bytes_and_leaves_the_source_pair() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("documents/filed/forms")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/filed/forms")).unwrap();
        std::fs::write(dir.path().join("blobs/notes/a.pdf.yaml"), "pointer").unwrap();
        std::fs::write(dir.path().join("documents/notes/a.pdf"), "pdf-bytes").unwrap();
        std::fs::write(dir.path().join("documents/filed/forms/a.pdf"), "other").unwrap();

        assert!(move_local_pointer(dir.path(), "notes/a.pdf", "filed/forms/a.pdf").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("blobs/notes/a.pdf.yaml")).unwrap(),
            "pointer"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("documents/notes/a.pdf")).unwrap(),
            "pdf-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("documents/filed/forms/a.pdf")).unwrap(),
            "other"
        );
        assert!(!dir.path().join("blobs/filed/forms/a.pdf.yaml").exists());
    }

    #[test]
    fn a_move_refuses_an_occupied_pointer_and_leaves_source_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("documents/filed/forms")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/filed/forms")).unwrap();
        std::fs::write(dir.path().join("blobs/notes/a.pdf.yaml"), "pointer").unwrap();
        std::fs::write(dir.path().join("documents/notes/a.pdf"), "pdf-bytes").unwrap();
        std::fs::write(dir.path().join("blobs/filed/forms/a.pdf.yaml"), "other").unwrap();

        assert!(move_local_pointer(dir.path(), "notes/a.pdf", "filed/forms/a.pdf").is_err());
        assert!(dir.path().join("blobs/notes/a.pdf.yaml").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("documents/notes/a.pdf")).unwrap(),
            "pdf-bytes"
        );
    }

    #[tokio::test]
    async fn reslug_checks_local_collisions_before_changing_the_live_identity() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("documents/filed/forms")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/filed/forms")).unwrap();
        std::fs::write(dir.path().join("blobs/notes/a.pdf.yaml"), "pointer").unwrap();
        std::fs::write(dir.path().join("documents/notes/a.pdf"), "bytes").unwrap();
        std::fs::write(dir.path().join("documents/filed/forms/a.pdf"), "collision").unwrap();
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}"
            )))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "previous_slug": "notes/a.pdf", "dry_run": true
            })))
            .expect(2)
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let error = reslug_with_local(&client, asset_id, "filed/forms/a.pdf", dir.path(), false)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("already exists"), "{error:#}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);

        std::fs::remove_file(dir.path().join("documents/filed/forms/a.pdf")).unwrap();
        std::fs::write(
            dir.path().join("blobs/filed/forms/a.pdf.yaml"),
            "pointer collision",
        )
        .unwrap();
        let error = reslug_with_local(&client, asset_id, "filed/forms/a.pdf", dir.path(), false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error:#}");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        assert_eq!(
            std::fs::read(dir.path().join("documents/notes/a.pdf")).unwrap(),
            b"bytes"
        );
        assert!(dir.path().join("blobs/notes/a.pdf.yaml").exists());
    }

    #[tokio::test]
    async fn put_pages_sends_the_manifest_block_and_reports_the_stored_count() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let manifest = crate::projects::manifest::parse(
            "pages:\n  - slug: timeline\n    kind: docket_deadline_board\n    title: Timeline\n    \
             lenses:\n      lawyer: [deadline_board, boundary_note, provenance_statement]\n",
        )
        .unwrap();
        let body = serde_json::json!({ "pages": [{
            "slug": "timeline",
            "kind": "docket_deadline_board",
            "title": "Timeline",
            "lenses": { "lawyer": ["deadline_board", "boundary_note", "provenance_statement"] }
        }]});
        Mock::given(method("PUT"))
            .and(path(format!("/app/api/projects/{project_id}/pages")))
            .and(body_json(body.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        assert_eq!(client.put_pages(&manifest.pages).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn put_matter_sends_the_manifest_block_or_null_to_clear() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let matter = word::matter::Matter {
            case_number: "00CV000000".into(),
            ..word::matter::Matter::default()
        };
        let sent = serde_json::json!({ "matter": serde_json::to_value(&matter).unwrap() });
        Mock::given(method("PUT"))
            .and(path(format!("/app/api/projects/{project_id}/matter")))
            .and(body_json(sent.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(sent))
            .expect(1)
            .mount(&server)
            .await;
        let cleared = serde_json::json!({ "matter": null });
        Mock::given(method("PUT"))
            .and(path(format!("/app/api/projects/{project_id}/matter")))
            .and(body_json(cleared.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(cleared))
            .expect(1)
            .mount(&server)
            .await;
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        assert!(client.put_matter(Some(&matter)).await.unwrap());
        assert!(!client.put_matter(None).await.unwrap());
    }

    #[tokio::test]
    async fn a_refused_pages_set_names_the_server_finding() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        Mock::given(method("PUT"))
            .and(path(format!("/app/api/projects/{project_id}/pages")))
            .respond_with(ResponseTemplate::new(400).set_body_string("D003: missing boundary_note"))
            .mount(&server)
            .await;
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let error = client.put_pages(&[]).await.unwrap_err();
        assert!(error.to_string().contains("D003"), "{error:#}");
    }

    #[tokio::test]
    async fn a_refused_live_reslug_leaves_the_local_pair_in_place() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/notes")).unwrap();
        std::fs::write(dir.path().join("blobs/notes/a.pdf.yaml"), "pointer").unwrap();
        std::fs::write(dir.path().join("documents/notes/a.pdf"), "bytes").unwrap();
        let endpoint = format!("/app/api/projects/{project_id}/documents/{asset_id}");
        Mock::given(method("PATCH"))
            .and(path(endpoint.clone()))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "previous_slug": "notes/a.pdf", "dry_run": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(endpoint))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": false
            })))
            .respond_with(ResponseTemplate::new(409).set_body_string("slug taken"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": asset_id, "byte_size": 5, "sha256_hex": "a", "filename": "a.pdf",
                 "kind": "memo", "visibility": "internal", "slug": "notes/a.pdf",
                 "inserted_at": "2026-10-01T00:00:00Z"}
            ])))
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        assert!(
            reslug_with_local(&client, asset_id, "filed/forms/a.pdf", dir.path(), false)
                .await
                .is_err()
        );

        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        assert!(dir.path().join("blobs/notes/a.pdf.yaml").exists());
        assert_eq!(
            std::fs::read(dir.path().join("documents/notes/a.pdf")).unwrap(),
            b"bytes"
        );
        assert!(!dir.path().join("documents/filed/forms/a.pdf").exists());
    }

    #[tokio::test]
    async fn an_interrupted_reslug_response_is_resolved_from_the_live_inventory() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/notes")).unwrap();
        std::fs::write(dir.path().join("blobs/notes/a.pdf.yaml"), "pointer").unwrap();
        std::fs::write(dir.path().join("documents/notes/a.pdf"), "bytes").unwrap();
        let endpoint = format!("/app/api/projects/{project_id}/documents/{asset_id}");
        Mock::given(method("PATCH"))
            .and(path(endpoint.clone()))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "previous_slug": "notes/a.pdf", "dry_run": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(endpoint))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": false
            })))
            .respond_with(ResponseTemplate::new(503).set_body_string("response lost"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": asset_id, "byte_size": 5, "sha256_hex": "a", "filename": "a.pdf",
                 "kind": "memo", "visibility": "internal", "slug": "filed/forms/a.pdf",
                 "inserted_at": "2026-10-01T00:00:00Z"}
            ])))
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let response = reslug_with_local(&client, asset_id, "filed/forms/a.pdf", dir.path(), false)
            .await
            .unwrap();

        assert_eq!(response["recovered_after_interruption"], true);
        assert!(!dir.path().join("blobs/notes/a.pdf.yaml").exists());
        assert_eq!(
            std::fs::read(dir.path().join("documents/filed/forms/a.pdf")).unwrap(),
            b"bytes"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_local_move_failure_compensates_the_live_identity() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/notes")).unwrap();
        std::fs::write(dir.path().join("blobs/notes/a.pdf.yaml"), "pointer").unwrap();
        std::fs::write(dir.path().join("documents/notes/a.pdf"), "bytes").unwrap();
        std::fs::create_dir_all(dir.path().join("documents")).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs")).unwrap();
        std::fs::write(
            dir.path().join("documents/filed"),
            "blocks target directory",
        )
        .unwrap();
        let endpoint = format!("/app/api/projects/{project_id}/documents/{asset_id}");
        Mock::given(method("PATCH"))
            .and(path(endpoint.clone()))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "previous_slug": "notes/a.pdf", "dry_run": true
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(endpoint.clone()))
            .and(body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "reslug": true, "dry_run": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "filed/forms/a.pdf", "previous_slug": "notes/a.pdf", "dry_run": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(endpoint))
            .and(body_json(serde_json::json!({
                "slug": "notes/a.pdf", "reslug": true, "dry_run": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "notes/a.pdf", "previous_slug": "filed/forms/a.pdf", "dry_run": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let error = reslug_with_local(&client, asset_id, "filed/forms/a.pdf", dir.path(), false)
            .await
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("live identity was restored"),
            "{error:#}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        assert!(dir.path().join("blobs/notes/a.pdf.yaml").exists());
        assert_eq!(
            std::fs::read(dir.path().join("documents/notes/a.pdf")).unwrap(),
            b"bytes"
        );
        assert_eq!(
            std::fs::read(dir.path().join("documents/filed")).unwrap(),
            b"blocks target directory"
        );
    }

    #[tokio::test]
    async fn retirement_requires_confirmation_and_sends_a_manual_delete() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let previous_id = Uuid::now_v7();
        let rows = serde_json::json!([
            {"id": asset_id, "byte_size": 5, "sha256_hex": "a", "filename": "note.txt",
             "kind": "memo", "visibility": "internal", "slug": "notes/note.txt",
             "inserted_at": "2026-10-01T00:00:00Z"},
            {"id": previous_id, "byte_size": 4, "sha256_hex": "b", "filename": "note.txt",
             "kind": "memo", "visibility": "internal", "slug": "notes/note.txt",
             "inserted_at": "2026-09-30T00:00:00Z"}
        ]);
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(200).set_body_json(rows))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}/delete"
            )))
            .and(body_json(serde_json::json!({ "manual": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "notes/note.txt", "revisions": 2, "changed": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let preview = retire_document(&client, asset_id, true, false)
            .await
            .unwrap();
        assert_eq!(preview["slug"], "notes/note.txt");
        assert_eq!(preview["revisions"], 2);
        assert!(retire_document(&client, asset_id, false, false)
            .await
            .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);

        let retired = retire_document(&client, asset_id, false, true)
            .await
            .unwrap();
        assert_eq!(retired["changed"], true);
        let requests = server.received_requests().await.unwrap();
        let deletion = requests
            .iter()
            .find(|request| request.method == "POST")
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&deletion.body).unwrap(),
            serde_json::json!({ "manual": true })
        );
    }

    // ---- LAW-102: `document upload` files a revision of the chain whose
    // sibling pointer sits beside `--file`, never a stray new chain. ----

    const SIBLING_POINTER: &str = "kind: unclassified\n\
        visibility: internal\n\
        current_version:\n  version: 1\n  asset_id: 0190a0a0-0000-7000-8000-000000000001\n  \
        created_at: '2026-09-05T12:00:00Z'\n  \
        sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  \
        size_bytes: 5\n";

    /// What one `document upload` did against a mock server: its exit code,
    /// the `slug` of every document POST (`None` when the body had none), and
    /// the checkout so a test can read the pointers it left.
    struct UploadProbe {
        exit: ExitCode,
        posted_slugs: Vec<Option<String>>,
        dir: tempfile::TempDir,
    }

    /// Lay `files` (path, contents) into a checkout for project `acme`, then
    /// upload `file` with `root` (relative to the checkout) as the working
    /// directory the command ran from.
    async fn probe_upload(
        files: &[(&str, &str)],
        root: &str,
        file: &str,
        slug: Option<&str>,
    ) -> UploadProbe {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let server_uri = server.uri();
        let _env = CredentialsEnv::new(&server_uri);
        let project_id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "unclassified",
                "visibility": "internal",
                "current_version": {
                    "version": 2,
                    "asset_id": Uuid::now_v7(),
                    "created_at": "2026-09-06T12:00:00Z",
                    "sha256": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
                    "size_bytes": 5
                },
                "previous_version": "0190a0a0-0000-7000-8000-000000000001"
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();
        for (relative, contents) in files {
            let target = dir.path().join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        // `ABS:` names a path below the checkout as an absolute `--file`.
        let file = file.strip_prefix("ABS:").map_or_else(
            || std::path::PathBuf::from(file),
            |relative| dir.path().join(relative),
        );
        let exit = document_upload_from(
            &dir.path().join(root),
            Some(server_uri.as_str()),
            "acme",
            &file,
            "unclassified",
            None,
            None,
            Some("text/markdown"),
            slug,
            None,
            &DocumentExecution::default(),
            None,
        )
        .await;
        let posted_slugs = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .map(|request| {
                serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["slug"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        UploadProbe {
            exit,
            posted_slugs,
            dir,
        }
    }

    async fn batch_probe(
        statuses: Vec<u16>,
        expired: bool,
        login_ok: bool,
    ) -> (ExitCode, usize, Vec<String>, bool) {
        batch_probe_files(
            statuses,
            expired,
            login_ok,
            &[("a.pdf", "a"), ("b.pdf", "a"), ("c.PNG", "c")],
            "documents/exhibits",
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn batch_probe_files(
        statuses: Vec<u16>,
        expired: bool,
        login_ok: bool,
        files: &[(&str, &str)],
        directory: &str,
    ) -> (ExitCode, usize, Vec<String>, bool) {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let base = server.uri();
        let _env = CredentialsEnv::new(&base);
        if expired {
            let path = credentials::default_credentials_path();
            let mut creds = credentials::load(&path).unwrap();
            creds.hosts.get_mut(&base).unwrap().expires_at = 0;
            credentials::save(&path, &creds).unwrap();
        }
        let project_id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        Mock::given(method("POST"))
            .respond_with(move |request: &wiremock::Request| {
                let index = count.fetch_add(1, Ordering::SeqCst);
                if expired || (index > 0 && statuses.get(index - 1) == Some(&401)) {
                    assert_eq!(request.headers.get("authorization").unwrap(), "Bearer renewed-token");
                }
                let status = statuses.get(index).copied().unwrap_or(201);
                if status == 202 { return ResponseTemplate::new(201).set_body_string("invalid pointer"); }
                ResponseTemplate::new(status).set_body_json(serde_json::json!({
                    "kind": "exhibit", "visibility": "internal",
                    "current_version": {"version": 1, "asset_id": Uuid::now_v7(),
                        "created_at": "2026-09-06T12:00:00Z",
                        "sha256": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
                        "size_bytes": 1}
                }))
            }).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.com\n  name: acme\n",
        )
        .unwrap();
        let uploads = dir.path().join(directory);
        std::fs::create_dir_all(&uploads).unwrap();
        for (name, bytes) in files {
            std::fs::write(uploads.join(name), bytes).unwrap();
        }
        let logins = Arc::new(AtomicUsize::new(0));
        let login_count = logins.clone();
        let exit = super::document_upload_directory_from(
            dir.path(),
            Some(&base),
            "acme",
            &uploads,
            "exhibit",
            Some("internal"),
            None,
            None,
            None,
            &DocumentExecution::default(),
            None,
            move |base| {
                login_count.fetch_add(1, Ordering::SeqCst);
                async move {
                    if !login_ok {
                        return Err(anyhow::anyhow!("login cancelled"));
                    }
                    let path = credentials::default_credentials_path();
                    let mut creds = credentials::load(&path)?;
                    let credential = creds.hosts.get_mut(&base).unwrap();
                    credential.token = "renewed-token".into();
                    credential.expires_at = super::now_secs() + 3600;
                    credentials::save(&path, &creds)
                }
            },
        )
        .await;
        let posted = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| {
                serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["slug"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        (
            exit,
            logins.load(Ordering::SeqCst),
            posted,
            dir.path().join("blobs/exhibits/a.pdf.yaml").exists(),
        )
    }

    #[tokio::test]
    async fn batch_upload_refuses_colliding_slugs_instead_of_filing_a_second_revision() {
        let (exit, logins, posted, _) = batch_probe_files(
            vec![],
            false,
            true,
            &[(" a.pdf", "first"), ("a.pdf", "different")],
            "incoming",
        )
        .await;
        assert_ne!(exit, ExitCode::SUCCESS);
        assert_eq!(logins, 0);
        assert_eq!(posted, ["a.pdf"]);
    }

    #[tokio::test]
    async fn batch_upload_skips_identical_bytes_and_preserves_extensions_and_pointers() {
        let (exit, logins, posted, pointer) = batch_probe(vec![], false, true).await;
        assert_eq!(exit, ExitCode::SUCCESS);
        assert_eq!(logins, 0);
        assert_eq!(posted, ["exhibits/a.pdf", "exhibits/c.PNG"]);
        assert!(pointer);
    }

    #[tokio::test]
    async fn batch_upload_does_not_resend_failed_duplicate_bytes_or_reauthenticate_forbidden() {
        for status in [202, 403, 500] {
            let (exit, logins, posted, pointer) = batch_probe(vec![status, 201], false, true).await;
            assert_ne!(exit, ExitCode::SUCCESS);
            assert_eq!(logins, 0);
            assert_eq!(posted, ["exhibits/a.pdf", "exhibits/c.PNG"]);
            assert!(!pointer);
        }
    }

    #[tokio::test]
    async fn batch_upload_recovers_expiry_before_or_during_batch_once() {
        for (statuses, expired, expected) in [
            (vec![], true, vec!["exhibits/a.pdf", "exhibits/c.PNG"]),
            (
                vec![201, 401, 201],
                false,
                vec!["exhibits/a.pdf", "exhibits/c.PNG", "exhibits/c.PNG"],
            ),
        ] {
            let (exit, logins, posted, pointer) = batch_probe(statuses, expired, true).await;
            assert_eq!(exit, ExitCode::SUCCESS);
            assert_eq!(logins, 1);
            assert_eq!(posted, expected);
            assert!(pointer);
        }
    }

    #[tokio::test]
    async fn batch_upload_preserves_progress_and_fails_on_unfiled_files_without_retrying_ambiguous_success(
    ) {
        for statuses in [vec![201, 500], vec![201, 202], vec![201, 401, 401]] {
            let (exit, logins, posted, pointer) = batch_probe(statuses.clone(), false, true).await;
            assert_ne!(exit, ExitCode::SUCCESS);
            assert_eq!(logins, usize::from(statuses.contains(&401)));
            assert_eq!(posted.len(), statuses.len());
            assert!(pointer);
        }
        let (exit, logins, posted, _) = batch_probe(vec![], true, false).await;
        assert_ne!(exit, ExitCode::SUCCESS);
        assert_eq!(logins, 1);
        assert!(posted.is_empty());
    }

    const DRAFTS_CHECKOUT: [(&str, &str); 2] = [
        ("documents/drafts/n.md", "draft"),
        ("blobs/drafts/n.md.yaml", SIBLING_POINTER),
    ];

    fn assert_files_a_revision_of(probe: &UploadProbe, slug: &str) {
        assert_eq!(probe.exit, ExitCode::SUCCESS);
        assert_eq!(probe.posted_slugs, vec![Some(slug.to_string())]);
    }

    fn assert_refused_before_any_upload(probe: &UploadProbe) {
        assert_ne!(probe.exit, ExitCode::SUCCESS);
        assert!(
            probe.posted_slugs.is_empty(),
            "a refused upload must not file anything, posted {:?}",
            probe.posted_slugs
        );
    }

    /// Nick's exact repro: the pointer is `documents/drafts/<n>.md.yaml`, the
    /// upload names `documents/drafts/<n>.md` and no `--slug`.
    #[tokio::test(flavor = "current_thread")]
    async fn upload_beside_a_pointer_files_a_revision_of_its_chain() {
        let probe = probe_upload(&DRAFTS_CHECKOUT, "", "ABS:documents/drafts/n.md", None).await;
        assert_files_a_revision_of(&probe, "drafts/n.md");
        assert!(
            !probe.dir.path().join("blobs/n.md.yaml").exists(),
            "no stray root pointer"
        );
        let pointer =
            std::fs::read_to_string(probe.dir.path().join("blobs/drafts/n.md.yaml")).unwrap();
        assert!(pointer.contains("version: 2"), "{pointer}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_from_a_subfolder_still_uses_the_pointers_slug() {
        let probe = probe_upload(
            &DRAFTS_CHECKOUT,
            "documents/drafts",
            "ABS:documents/drafts/n.md",
            None,
        )
        .await;
        assert_files_a_revision_of(&probe, "drafts/n.md");
        assert!(!probe.dir.path().join("documents/drafts/documents").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_from_an_unrelated_folder_still_uses_the_pointers_slug() {
        let mut files = DRAFTS_CHECKOUT.to_vec();
        files.push(("scratch/keep", ""));
        let probe = probe_upload(&files, "scratch", "ABS:documents/drafts/n.md", None).await;
        assert_files_a_revision_of(&probe, "drafts/n.md");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_through_dot_dot_still_uses_the_pointers_slug() {
        let mut files = DRAFTS_CHECKOUT.to_vec();
        files.push(("documents/other/keep", ""));
        let probe = probe_upload(&files, "", "ABS:documents/other/../drafts/n.md", None).await;
        assert_files_a_revision_of(&probe, "drafts/n.md");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_of_a_documents_root_file_names_its_pointers_slug() {
        let files = [
            ("documents/summons.pdf", "draft"),
            ("blobs/summons.pdf.yaml", SIBLING_POINTER),
        ];
        let probe = probe_upload(&files, "", "ABS:documents/summons.pdf", None).await;
        assert_files_a_revision_of(&probe, "summons.pdf");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_beside_a_retired_yml_pointer_is_refused_before_filing() {
        let files = [
            ("documents/drafts/n.md", "draft"),
            ("blobs/drafts/n.md.yml", SIBLING_POINTER),
        ];
        let probe = probe_upload(&files, "", "ABS:documents/drafts/n.md", None).await;
        assert_refused_before_any_upload(&probe);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_beside_both_pointer_spellings_is_refused() {
        let files = [
            ("documents/drafts/n.md", "draft"),
            ("blobs/drafts/n.md.yaml", SIBLING_POINTER),
            ("blobs/drafts/n.md.yml", SIBLING_POINTER),
        ];
        let probe = probe_upload(&files, "", "ABS:documents/drafts/n.md", None).await;
        assert_refused_before_any_upload(&probe);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_with_the_pointers_own_slug_is_accepted() {
        let probe = probe_upload(
            &DRAFTS_CHECKOUT,
            "",
            "ABS:documents/drafts/n.md",
            Some("drafts/n.md"),
        )
        .await;
        assert_files_a_revision_of(&probe, "drafts/n.md");
    }

    /// The bug: a `--slug` that differs from the sibling pointer's silently
    /// started a second chain. It must be refused, naming both slugs.
    #[tokio::test(flavor = "current_thread")]
    async fn upload_with_a_different_slug_than_the_pointer_is_refused() {
        let probe = probe_upload(
            &DRAFTS_CHECKOUT,
            "",
            "ABS:documents/drafts/n.md",
            Some("n.md"),
        )
        .await;
        assert_refused_before_any_upload(&probe);
        assert!(!probe.dir.path().join("blobs/n.md.yaml").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_beside_a_malformed_pointer_is_refused_not_a_new_chain() {
        let files = [
            ("documents/drafts/n.md", "draft"),
            (
                "blobs/drafts/n.md.yaml",
                "kind: [unterminated
",
            ),
        ];
        let probe = probe_upload(&files, "", "ABS:documents/drafts/n.md", None).await;
        assert_refused_before_any_upload(&probe);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_beside_a_pointer_that_is_a_directory_is_refused() {
        let files = [
            ("documents/drafts/n.md", "draft"),
            ("blobs/drafts/n.md.yaml/keep", ""),
        ];
        let probe = probe_upload(&files, "", "ABS:documents/drafts/n.md", None).await;
        assert_refused_before_any_upload(&probe);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_with_no_sibling_pointer_keeps_the_folder_slug() {
        let files = [("documents/drafts/new.md", "draft")];
        let probe = probe_upload(&files, "", "ABS:documents/drafts/new.md", None).await;
        assert_files_a_revision_of(&probe, "drafts/new.md");
    }

    /// The checkout is the nearest matching manifest above `--file`, so a
    /// first upload typed from elsewhere stages its pointer in that checkout.
    #[tokio::test(flavor = "current_thread")]
    async fn upload_with_no_pointer_from_elsewhere_stages_in_the_files_checkout() {
        let files = [("documents/drafts/new.md", "draft"), ("scratch/keep", "")];
        let probe = probe_upload(&files, "scratch", "ABS:documents/drafts/new.md", None).await;
        assert_files_a_revision_of(&probe, "drafts/new.md");
        assert!(probe.dir.path().join("blobs/drafts/new.md.yaml").is_file());
        assert!(!probe.dir.path().join("scratch/documents").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_outside_documents_ignores_a_neighbouring_yaml() {
        let files = [
            ("scratch/x.md", "draft"),
            (
                "scratch/x.md.yaml",
                "not: a pointer
",
            ),
        ];
        let probe = probe_upload(&files, "", "ABS:scratch/x.md", None).await;
        assert_eq!(probe.exit, ExitCode::SUCCESS);
        assert_eq!(probe.posted_slugs, vec![None]);
    }

    /// `CodeQL` treats every value read from the parsed CLI enum as a secret, so
    /// the merge report line must never interpolate `keep` or `drop`.
    #[test]
    fn merge_report_line_never_echoes_parsed_arguments() {
        let source = include_str!("remote.rs");
        let start = source
            .find("pub async fn merge_people")
            .expect("merge_people");
        let end = source[start..].find("\n}\n").expect("end of merge_people") + start;
        let body = &source[start..end];
        for line in body.lines().filter(|line| line.contains("println!")) {
            assert!(
                !line.contains("{keep}")
                    && !line.contains("{drop}")
                    && !line.contains("keep,")
                    && !line.contains("drop,"),
                "merge output must not echo parsed arguments: {line}"
            );
        }
    }

    /// Lane routing is inclusive at the per-kind cap: everything that fits
    /// the buffered door today stays there, and only the sizes the buffered
    /// door rejects move to the streaming lane.
    #[test]
    fn upload_lane_routes_at_the_per_kind_cap() {
        for kind in ["unclassified", "transcript"] {
            let cap = store::documents::max_document_upload_bytes(kind);
            assert_eq!(
                super::upload_lane_for(cap, kind),
                super::UploadLane::Buffered,
                "{kind} at its cap rides the buffered door"
            );
            assert_eq!(
                super::upload_lane_for(cap + 1, kind),
                super::UploadLane::Streaming,
                "{kind} one byte over its cap moves to the streaming lane"
            );
            assert_eq!(
                super::upload_lane_for(store::documents::MAX_STREAMING_UPLOAD_BYTES, kind),
                super::UploadLane::Streaming,
                "{kind} at the asset ceiling still has a lane"
            );
        }
    }

    #[tokio::test]
    async fn document_client_refuses_remote_plaintext_before_request() {
        let error = DocumentClient::with_credential(
            "http://upload.example.test".into(),
            "synthetic-token".into(),
            "acme",
        )
        .await
        .expect_err("a remote plaintext origin must be refused");
        assert!(
            error
                .to_string()
                .contains("authenticated transfers require HTTPS"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn document_client_does_not_follow_deployment_redirects() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/redirected", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/redirected"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::now_v7(), "code": "acme"}
            ])))
            .expect(0)
            .mount(&server)
            .await;
        assert!(
            DocumentClient::with_credential(server.uri(), "synthetic-token".into(), "acme")
                .await
                .is_err()
        );
    }

    async fn streaming_client_fixture(bytes: &[u8]) -> (MockServer, DocumentClient, Uuid, Uuid) {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        // `with_credential` resolves the project code before anything else,
        // so the mock deployment must answer the projects list first.
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        let client =
            DocumentClient::with_credential(server.uri(), "test-token".to_string(), "acme")
                .await
                .expect("a client against the mock deployment");

        let session_body = serde_json::json!({
            "id": session_id,
            "chunk_size": 4,
            "received_bytes": 0
        });
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session"
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(session_body))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session/complete"
            )))
            .and(body_json(serde_json::json!({ "session_id": session_id })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "kind": "transcript",
                "visibility": "internal",
                "current_version": {
                    "version": 1,
                    "asset_id": Uuid::now_v7(),
                    "created_at": "2026-10-30T12:00:00Z",
                    "sha256": store::documents::sha256_hex(bytes),
                    "size_bytes": bytes.len()
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        (server, client, project_id, session_id)
    }

    /// The streaming client end to end against a mock deployment: open, two
    /// chunks, a 409 from a replayed chunk after a lost acknowledgement, the
    /// status-door re-sync, completion, and the same pointer the buffered
    /// door files. Four bytes per chunk makes the choreography explicit.
    #[tokio::test(flavor = "current_thread")]
    async fn document_upload_streams_in_chunks_and_resumes_after_a_conflict() {
        let bytes = b"eight bytes here".to_vec();
        let (server, client, project_id, session_id) = streaming_client_fixture(&bytes).await;
        let encode = |chunk: &[u8]| -> String {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(chunk)
        };
        let chunk_url = format!("/app/api/projects/{project_id}/documents/upload-session/chunk");
        // 16 bytes at a 4-byte chunk size stream as four chunks. The first
        // append of chunk one is refused with a conflict — the lost
        // acknowledgement case — so the client re-syncs from the status door
        // and re-sends chunk one at the server's offset. wiremock matches
        // equal-priority mocks in registration order, so the one-shot 409 is
        // registered before the acknowledging 200.
        Mock::given(method("POST"))
            .and(path(&chunk_url))
            .and(body_json(serde_json::json!({
                "session_id": session_id,
                "offset": 0,
                "content_base64": encode(&bytes[..4])
            })))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "chunk_offset_mismatch",
                "expected_offset": 0
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(&chunk_url))
            .and(body_json(serde_json::json!({
                "session_id": session_id,
                "offset": 0,
                "content_base64": encode(&bytes[..4])
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": session_id,
                "chunk_size": 4,
                "received_bytes": 4
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The re-sync reads the acknowledged offset from the status door; the
        // server stands at zero because the refused chunk was never applied.
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session/status"
            )))
            .and(body_json(serde_json::json!({ "session_id": session_id })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": session_id,
                "chunk_size": 4,
                "received_bytes": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        // The remaining chunks each land exactly once at their declared offset.
        for (index, chunk) in bytes.chunks(4).enumerate().skip(1) {
            Mock::given(method("POST"))
                .and(path(&chunk_url))
                .and(body_json(serde_json::json!({
                    "session_id": session_id,
                    "offset": index * 4,
                    "content_base64": encode(chunk)
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": session_id,
                    "chunk_size": 4,
                    "received_bytes": (index + 1) * 4
                })))
                .expect(1)
                .mount(&server)
                .await;
        }

        let pointer = client
            .upload_bytes_streaming(
                "hearing.mp3",
                &bytes,
                "transcript",
                None,
                None,
                Some("audio/mpeg"),
                None,
                None,
                None,
                Some("machine"),
                &DocumentExecution::default(),
            )
            .await
            .expect("the streaming upload completes");
        assert_eq!(pointer.current_version.size_bytes, 16);
        assert_eq!(
            pointer.current_version.sha256,
            store::documents::sha256_hex(&bytes)
        );
    }

    /// A pointer body as the document doors answer it.
    fn pointer_json(bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "kind": "unclassified",
            "visibility": "internal",
            "current_version": {
                "version": 1,
                "asset_id": Uuid::now_v7(),
                "created_at": "2026-10-30T12:00:00Z",
                "sha256": store::documents::sha256_hex(bytes),
                "size_bytes": bytes.len()
            }
        })
    }

    fn session_json(session_id: Uuid, chunk_size: u64, received: u64) -> serde_json::Value {
        serde_json::json!({
            "id": session_id,
            "chunk_size": chunk_size,
            "received_bytes": received
        })
    }

    /// The JSON body of the only request the server saw on `route`.
    async fn only_body(server: &MockServer, route: &str) -> serde_json::Value {
        let requests = server.received_requests().await.unwrap();
        let matching: Vec<_> = requests
            .iter()
            .filter(|request| request.url.path() == route)
            .collect();
        assert_eq!(matching.len(), 1, "one request to {route}");
        serde_json::from_slice(&matching[0].body).unwrap()
    }

    #[tokio::test]
    async fn retention_commands_post_their_verb_and_body_and_name_a_refusal() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let door =
            |verb: &str| format!("/app/api/projects/{project_id}/documents/{asset_id}/{verb}");

        Mock::given(method("POST"))
            .and(path(door("delete")))
            .and(body_json(serde_json::json!({ "commit_sha": "abc123" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "deleted": 3 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(door("delete")))
            .and(body_json(serde_json::json!({ "manual": true })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "deleted": 1 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(door("restore")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "restored": 2 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(door("hold")))
            .and(body_json(serde_json::json!({ "held": true })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "held": true })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(door("hold")))
            .and(body_json(serde_json::json!({ "held": false })))
            .respond_with(ResponseTemplate::new(409).set_body_string("\n  chain is not held\n"))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            client.soft_delete(asset_id, "abc123").await.unwrap()["deleted"],
            3
        );
        assert_eq!(client.retire(asset_id).await.unwrap()["deleted"], 1);
        assert_eq!(client.restore(asset_id).await.unwrap()["restored"], 2);
        assert_eq!(
            client.set_legal_hold(asset_id, true).await.unwrap()["held"],
            true
        );
        let refusal = client
            .set_legal_hold(asset_id, false)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            refusal,
            "document hold failed: 409 Conflict: chain is not held"
        );
        // The restore door carries no body at all.
        let requests = server.received_requests().await.unwrap();
        let restore = requests
            .iter()
            .find(|request| request.url.path() == door("restore"))
            .unwrap();
        assert!(restore.body.is_empty());
    }

    #[tokio::test]
    async fn a_retention_answer_that_is_not_json_is_a_parse_error() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/{asset_id}/restore"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let error = DocumentClient::for_check_tests(server.uri(), project_id)
            .restore(asset_id)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("parse document restore response"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn document_reads_return_their_bodies_and_name_each_refusal() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let missing = Uuid::now_v7();
        let authority_id = Uuid::now_v7();
        let refused_authority = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);

        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/revisions"
            )))
            .and(query_param("slug", "gone.pdf"))
            .respond_with(ResponseTemplate::new(404).set_body_string("no such chain"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(500).set_body_string(""))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/service")))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden\nmore"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/projects/acme/documents/{asset_id}/download"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"revision bytes".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/projects/acme/documents/{missing}/download"
            )))
            .respond_with(ResponseTemplate::new(404).set_body_string("not visible"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/authorities/{authority_id}/archive")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"archived".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/authorities/{refused_authority}/archive"
            )))
            .respond_with(ResponseTemplate::new(403).set_body_string("not yours"))
            .mount(&server)
            .await;

        assert_eq!(
            client
                .list_revisions("gone.pdf")
                .await
                .unwrap_err()
                .to_string(),
            "no revision of `gone.pdf` is visible on this matter: 404 Not Found: no such chain"
        );
        assert_eq!(
            client.list_documents().await.unwrap_err().to_string(),
            "list documents failed: 500 Internal Server Error: (empty response)"
        );
        assert_eq!(
            client.list_service().await.unwrap_err().to_string(),
            "list service failed: 403 Forbidden: forbidden"
        );
        assert_eq!(
            client.download_revision(asset_id).await.unwrap(),
            b"revision bytes"
        );
        assert_eq!(
            client
                .download_revision(missing)
                .await
                .unwrap_err()
                .to_string(),
            format!("download of revision {missing} failed: 404 Not Found: not visible")
        );
        assert_eq!(
            client
                .download_authority_archive(authority_id)
                .await
                .unwrap(),
            b"archived"
        );
        assert_eq!(
            client
                .download_authority_archive(refused_authority)
                .await
                .unwrap_err()
                .to_string(),
            format!("download of authority {refused_authority} failed: 403 Forbidden: not yours")
        );
    }

    #[tokio::test]
    async fn document_reads_parse_a_successful_inventory_and_service_list() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/service")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/revisions"
            )))
            .and(query_param("slug", "letter.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        assert!(client.list_documents().await.unwrap().is_empty());
        assert!(client.list_service().await.unwrap().is_empty());
        let error = client.list_revisions("letter.pdf").await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("parse document revisions response"),
            "{error:#}"
        );
        assert_eq!(client.project_id(), project_id);
        assert_eq!(client.base(), server.uri());
        assert_eq!(client.token(), "test-token");
    }

    #[tokio::test]
    async fn document_patch_doors_send_their_fields_and_name_each_refusal() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let row = format!("/app/api/projects/{project_id}/documents/{asset_id}");

        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(serde_json::json!({ "visibility": "client" })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(serde_json::json!({ "visibility": "secret" })))
            .respond_with(ResponseTemplate::new(422).set_body_string("unknown visibility"))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(serde_json::json!({
                "slug": "letter.pdf",
                "dry_run": false,
                "kind": "correspondence"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "slug": "letter.pdf" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(
                serde_json::json!({ "slug": "taken.pdf", "dry_run": true }),
            ))
            .respond_with(ResponseTemplate::new(409).set_body_string("slug taken"))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(
                serde_json::json!({ "kind": "unclassified", "dry_run": true }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "would_change": 2 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(
                serde_json::json!({ "kind": "bogus", "dry_run": false }),
            ))
            .respond_with(ResponseTemplate::new(422).set_body_string("unknown kind"))
            .mount(&server)
            .await;

        client.set_visibility(asset_id, "client").await.unwrap();
        assert_eq!(
            client
                .set_visibility(asset_id, "secret")
                .await
                .unwrap_err()
                .to_string(),
            "document visibility failed: 422 Unprocessable Entity: unknown visibility"
        );
        assert_eq!(
            client
                .assign_slug(asset_id, "letter.pdf", Some("correspondence"), false)
                .await
                .unwrap()["slug"],
            "letter.pdf"
        );
        assert_eq!(
            client
                .assign_slug(asset_id, "taken.pdf", None, true)
                .await
                .unwrap_err()
                .to_string(),
            "document slug failed: 409 Conflict: slug taken"
        );
        assert_eq!(
            client
                .reclassify_kind(asset_id, "unclassified", true)
                .await
                .unwrap()["would_change"],
            2
        );
        assert_eq!(
            client
                .reclassify_kind(asset_id, "bogus", false)
                .await
                .unwrap_err()
                .to_string(),
            "document kind failed: 422 Unprocessable Entity: unknown kind"
        );
    }

    #[tokio::test]
    async fn chain_moving_doors_name_each_refusal() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let row = format!("/app/api/projects/{project_id}/documents/{asset_id}");
        Mock::given(method("PATCH"))
            .and(path(&row))
            .and(body_json(serde_json::json!({
                "slug": "moved.pdf",
                "reslug": true,
                "dry_run": false
            })))
            .respond_with(ResponseTemplate::new(409).set_body_string("chain is held"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{row}/reclassify")))
            .respond_with(ResponseTemplate::new(403).set_body_string("lawyer only"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{row}/storage")))
            .and(body_json(serde_json::json!({ "dry_run": false })))
            .respond_with(ResponseTemplate::new(404).set_body_string("no same-hash sibling"))
            .mount(&server)
            .await;

        assert_eq!(
            client
                .reslug(asset_id, "moved.pdf", false)
                .await
                .unwrap_err()
                .to_string(),
            "document reslug failed: 409 Conflict: chain is held"
        );
        assert_eq!(
            client
                .reclassify_accepted_kind(asset_id, "correspondence", false)
                .await
                .unwrap_err()
                .to_string(),
            "document reclassify failed: 403 Forbidden: lawyer only"
        );
        assert_eq!(
            client
                .repair_storage(asset_id, false)
                .await
                .unwrap_err()
                .to_string(),
            "document repair failed: 404 Not Found: no same-hash sibling"
        );
    }

    #[tokio::test]
    async fn file_mail_posts_the_message_and_reports_a_refusal() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let message_id = Uuid::now_v7();
        let refused = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let door = format!("/app/api/projects/{project_id}/mail/file");
        Mock::given(method("POST"))
            .and(path(&door))
            .and(body_json(serde_json::json!({
                "message_id": message_id,
                "kind": "correspondence",
                "visibility": "client",
                "dry_run": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dry_run": false,
                "attachments": [
                    {"filename": "a.pdf", "content_type": "application/pdf", "size_bytes": 3}
                ],
                "filed": [pointer_json(b"abc")]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(&door))
            .and(body_json(serde_json::json!({
                "message_id": refused,
                "kind": "correspondence",
                "visibility": "client",
                "dry_run": true
            })))
            .respond_with(ResponseTemplate::new(404).set_body_string("no such message"))
            .mount(&server)
            .await;

        let filed = client
            .file_mail(message_id, "correspondence", "client", false)
            .await
            .unwrap();
        assert_eq!(filed.attachments[0].filename, "a.pdf");
        assert_eq!(filed.attachments[0].content_type, "application/pdf");
        assert_eq!(filed.attachments[0].size_bytes, 3);
        assert_eq!(filed.filed.len(), 1);
        assert_eq!(
            client
                .file_mail(refused, "correspondence", "client", true)
                .await
                .unwrap_err()
                .to_string(),
            format!("filing message {refused} failed: 404 Not Found: no such message")
        );
    }

    #[tokio::test]
    async fn a_buffered_upload_sends_every_optional_field_and_the_execution_record() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let door = format!("/app/api/projects/{project_id}/documents");
        Mock::given(method("POST"))
            .and(path(&door))
            .respond_with(ResponseTemplate::new(201).set_body_json(pointer_json(b"signed")))
            .expect(1)
            .mount(&server)
            .await;
        let signing = store::documents::Signing {
            method: store::documents::SigningMethod::WetInk,
            signed_on: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        };
        let notarization = store::documents::Notarization {
            method: store::documents::NotarizationMethod::InPerson,
            acknowledged_on: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            state: "NV".into(),
            county: "Clark".into(),
            commission_expires: chrono::NaiveDate::from_ymd_opt(2029, 1, 1).unwrap(),
        };
        let execution = DocumentExecution {
            signing: Some(signing),
            notarization: vec![notarization.clone()],
            evidence: None,
        };

        let pointer = client
            .upload_bytes_with_details(
                "deed.pdf",
                b"signed",
                "unclassified",
                Some("client"),
                Some("  the recorded deed  "),
                Some("application/pdf"),
                Some(" deed.pdf "),
                Some(serde_json::json!({ "commit": "abc" })),
                Some(serde_json::json!({ "asset_id": "source" })),
                Some("machine"),
                &execution,
            )
            .await
            .unwrap();
        assert_eq!(pointer.current_version.size_bytes, 6);

        let body = only_body(&server, &door).await;
        assert_eq!(body["filename"], "deed.pdf");
        assert_eq!(body["content_base64"], "c2lnbmVk");
        assert_eq!(body["content_type"], "application/pdf");
        assert_eq!(body["visibility"], "client");
        assert_eq!(body["description"], "the recorded deed");
        assert_eq!(body["slug"], "deed.pdf");
        assert_eq!(body["metadata"]["commit"], "abc");
        assert_eq!(body["derived_from"]["asset_id"], "source");
        assert_eq!(body["transcript_quality"], "machine");
        assert_eq!(body["signing"], serde_json::to_value(signing).unwrap());
        assert_eq!(
            body["notarization"],
            serde_json::to_value(vec![notarization]).unwrap()
        );
        assert!(body.get("evidence").is_none());
    }

    #[tokio::test]
    async fn a_buffered_upload_maps_401_to_an_expired_session_and_names_other_refusals() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        let door = format!("/app/api/projects/{project_id}/documents");
        Mock::given(method("POST"))
            .and(path(&door))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(&door))
            .respond_with(ResponseTemplate::new(413).set_body_string("too large"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(&door))
            .respond_with(ResponseTemplate::new(200).set_body_string("filed?"))
            .mount(&server)
            .await;

        let upload = || {
            client.upload_bytes(
                "a.txt",
                b"abc",
                "unclassified",
                None,
                Some("   "),
                None,
                None,
                None,
            )
        };
        let expired = upload().await.unwrap_err();
        assert!(expired.downcast_ref::<UploadSessionExpired>().is_some());
        assert_eq!(
            upload().await.unwrap_err().to_string(),
            "document upload failed: 413 Payload Too Large: too large"
        );
        let unparsed = upload().await.unwrap_err();
        assert!(
            unparsed
                .to_string()
                .contains("parse document pointer response"),
            "{unparsed:#}"
        );
        // Defaults apply when no content type, visibility, or real description is given.
        let requests = server.received_requests().await.unwrap();
        let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(first["content_type"], "application/octet-stream");
        assert_eq!(first["visibility"], "internal");
        assert!(first.get("description").is_none());
        assert!(first.get("slug").is_none());
    }

    /// Mount the session-open door answering `status` with `body`.
    async fn mount_session_open(
        server: &MockServer,
        project_id: Uuid,
        status: u16,
        body: serde_json::Value,
    ) {
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session"
            )))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn stream(client: &DocumentClient, bytes: &[u8]) -> anyhow::Result<()> {
        client
            .upload_bytes_streaming(
                "hearing.mp3",
                bytes,
                "transcript",
                Some("client"),
                Some("hearing audio"),
                Some("audio/mpeg"),
                Some("hearing.mp3"),
                Some(serde_json::json!({ "source": "court" })),
                Some(serde_json::json!({ "asset_id": "x" })),
                Some("machine"),
                &DocumentExecution::default(),
            )
            .await
            .map(|_| ())
    }

    #[tokio::test]
    async fn a_streaming_session_open_refusal_is_named_and_401_is_an_expired_session() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        mount_session_open(&server, project_id, 503, serde_json::json!("busy")).await;
        let error = stream(&client, b"abcd").await.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("streaming upload failed to open a session: 503"),
            "{error:#}"
        );
        // The open request declares the size and digest and every optional field.
        let body = only_body(
            &server,
            &format!("/app/api/projects/{project_id}/documents/upload-session"),
        )
        .await;
        assert_eq!(body["declared_size"], 4);
        assert_eq!(
            body["declared_sha256"],
            store::documents::sha256_hex(b"abcd")
        );
        assert_eq!(body["description"], "hearing audio");
        assert_eq!(body["slug"], "hearing.mp3");
        assert_eq!(body["metadata"]["source"], "court");
        assert_eq!(body["derived_from"]["asset_id"], "x");
        assert_eq!(body["transcript_quality"], "machine");
        assert_eq!(body["visibility"], "client");

        let server = MockServer::start().await;
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        mount_session_open(&server, project_id, 401, serde_json::json!({})).await;
        assert!(stream(&client, b"abcd")
            .await
            .unwrap_err()
            .downcast_ref::<UploadSessionExpired>()
            .is_some());
    }

    #[tokio::test]
    async fn a_streaming_chunk_refusal_names_the_offset_or_the_expired_session() {
        let project_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        for (status, expected) in [
            (410, "the upload session expired mid-upload"),
            (
                500,
                "chunk at offset 0 failed: 500 Internal Server Error: boom",
            ),
        ] {
            let server = MockServer::start().await;
            let client = DocumentClient::for_check_tests(server.uri(), project_id);
            mount_session_open(&server, project_id, 201, session_json(session_id, 4, 0)).await;
            Mock::given(method("POST"))
                .and(path(format!(
                    "/app/api/projects/{project_id}/documents/upload-session/chunk"
                )))
                .respond_with(ResponseTemplate::new(status).set_body_string("boom"))
                .mount(&server)
                .await;
            let error = stream(&client, b"abcdefgh").await.unwrap_err().to_string();
            assert!(error.starts_with(expected), "{status}: {error}");
        }

        let server = MockServer::start().await;
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        mount_session_open(&server, project_id, 201, session_json(session_id, 4, 0)).await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session/chunk"
            )))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        assert!(stream(&client, b"abcdefgh")
            .await
            .unwrap_err()
            .downcast_ref::<UploadSessionExpired>()
            .is_some());
    }

    #[tokio::test]
    async fn repeated_chunk_conflicts_stop_after_four_resyncs() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        mount_session_open(&server, project_id, 201, session_json(session_id, 4, 0)).await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session/chunk"
            )))
            .respond_with(ResponseTemplate::new(409))
            .expect(5)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session/status"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(session_json(session_id, 4, 0)))
            .expect(4)
            .mount(&server)
            .await;
        let error = stream(&client, b"abcdefgh").await.unwrap_err().to_string();
        assert!(
            error.starts_with("the server refused the chunk at offset 0 repeatedly"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_resync_against_a_gone_or_refusing_status_door_is_named() {
        let project_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        for (status, expected) in [
            (404, "the streaming upload session is gone"),
            (
                500,
                "upload session status failed: 500 Internal Server Error: down",
            ),
            (401, "the session for"),
        ] {
            let server = MockServer::start().await;
            let client = DocumentClient::for_check_tests(server.uri(), project_id);
            mount_session_open(&server, project_id, 201, session_json(session_id, 4, 0)).await;
            Mock::given(method("POST"))
                .and(path(format!(
                    "/app/api/projects/{project_id}/documents/upload-session/chunk"
                )))
                .respond_with(ResponseTemplate::new(409))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(format!(
                    "/app/api/projects/{project_id}/documents/upload-session/status"
                )))
                .respond_with(ResponseTemplate::new(status).set_body_string("down"))
                .mount(&server)
                .await;
            let error = stream(&client, b"abcdefgh").await.unwrap_err().to_string();
            assert!(error.starts_with(expected), "{status}: {error}");
        }
    }

    #[tokio::test]
    async fn an_acknowledgement_that_does_not_advance_stops_the_upload() {
        let server = MockServer::start().await;
        let project_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let client = DocumentClient::for_check_tests(server.uri(), project_id);
        mount_session_open(&server, project_id, 201, session_json(session_id, 4, 0)).await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/documents/upload-session/chunk"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(session_json(session_id, 4, 0)))
            .expect(1)
            .mount(&server)
            .await;
        let error = stream(&client, b"abcdefgh").await.unwrap_err().to_string();
        assert_eq!(
            error,
            "the server acknowledged 0 bytes after the chunk at offset 0; re-run the command to resume"
        );
    }

    #[tokio::test]
    async fn a_resumed_session_sends_only_the_unacknowledged_tail_then_names_a_completion_refusal()
    {
        let project_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        for (status, expired) in [(500, false), (401, true)] {
            let server = MockServer::start().await;
            let client = DocumentClient::for_check_tests(server.uri(), project_id);
            // The session already holds the first four bytes.
            mount_session_open(&server, project_id, 200, session_json(session_id, 4, 4)).await;
            Mock::given(method("POST"))
                .and(path(format!(
                    "/app/api/projects/{project_id}/documents/upload-session/chunk"
                )))
                .and(body_json(serde_json::json!({
                    "session_id": session_id,
                    "offset": 4,
                    "content_base64": "ZWZnaA=="
                })))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(session_json(session_id, 4, 8)),
                )
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(format!(
                    "/app/api/projects/{project_id}/documents/upload-session/complete"
                )))
                .respond_with(ResponseTemplate::new(status).set_body_string("not assembled"))
                .expect(1)
                .mount(&server)
                .await;
            let error = stream(&client, b"abcdefgh").await.unwrap_err();
            if expired {
                assert!(error.downcast_ref::<UploadSessionExpired>().is_some());
            } else {
                assert!(
                    error.to_string().starts_with(
                        "streaming upload completion failed: 500 Internal Server Error: not assembled"
                    ),
                    "{error:#}"
                );
            }
        }
    }

    #[tokio::test]
    async fn integration_door_parses_its_report_and_names_a_refusal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/api/integrations/slack/ensure"))
            .and(body_json(serde_json::json!({ "project_code": "acme" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{"project_code": "acme", "outcome": "created"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/integrations/notion/ensure"))
            .respond_with(ResponseTemplate::new(403).set_body_string("not an admin"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/integrations/surface/ensure"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;

        let report = super::post_integration_door(
            &server.uri(),
            "t",
            "/app/api/integrations/slack/ensure",
            serde_json::json!({ "project_code": "acme" }),
        )
        .await
        .unwrap();
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].outcome, "created");
        assert_eq!(
            super::post_integration_door(
                &server.uri(),
                "t",
                "/app/api/integrations/notion/ensure",
                serde_json::json!({}),
            )
            .await
            .unwrap_err()
            .to_string(),
            "integration door failed: 403 Forbidden: not an admin"
        );
        assert!(super::post_integration_door(
            &server.uri(),
            "t",
            "/app/api/integrations/surface/ensure",
            serde_json::json!({}),
        )
        .await
        .unwrap_err()
        .to_string()
        .starts_with("parse POST"));
    }

    #[tokio::test]
    async fn a_ci_scoped_dry_run_names_the_project_and_is_never_rechecked() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .expect(1)
            .mount(&server)
            .await;
        let session = (server.uri(), "t".to_string());
        let failure = super::send_chunk(
            &session,
            "person",
            "lookup_fields: [email]\nrecords: []\n",
            0,
            SeedOptions {
                overwrite: false,
                dry_run: true,
                as_ci_project: Some("acme"),
            },
        )
        .await
        .expect_err("a 502 dry run fails");
        assert_eq!(
            failure.into_error(0).to_string(),
            "seed person failed: 502 Bad Gateway: bad gateway"
        );
        let body = only_body(&server, "/app/api/seed").await;
        assert_eq!(body["as_ci"], true);
        assert_eq!(body["project"], "acme");
        assert_eq!(body["dry_run"], true);
        assert_eq!(body["overwrite"], false);
    }

    #[tokio::test]
    async fn a_seed_with_no_answer_reports_the_request_failure() {
        // A listener that is bound and then dropped leaves a port nothing answers on.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let session = (format!("http://127.0.0.1:{port}"), "t".to_string());
        let failure = super::send_chunk(
            &session,
            "person",
            "records: []\n",
            0,
            SeedOptions::default(),
        )
        .await
        .expect_err("no answer is a failure");
        let error = failure.into_error(0);
        assert!(
            error.to_string().starts_with("POST /app/api/seed:"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_small_seed_document_is_posted_whole_and_counts_its_records() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "created": 2 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let session = (server.uri(), "t".to_string());
        let yaml = "lookup_fields: [email]\nrecords:\n  - email: a@example.com\n  - email: b@example.com\n";
        let committed = super::post_seed(&session, "person", yaml, SeedOptions::default())
            .await
            .unwrap_or_else(|failure| panic!("{}", failure.into_error(0)));
        assert_eq!(committed, 2);
        assert_eq!(only_body(&server, "/app/api/seed").await["yaml"], yaml);
    }

    #[test]
    fn seed_helpers_read_counts_records_and_partial_commits() {
        assert_eq!(
            super::parse_seed_counts(r#"{"created":1,"updated":2}"#),
            super::SeedCounts {
                created: 1,
                updated: 2,
                unchanged: 0
            }
        );
        assert_eq!(
            super::parse_seed_counts("<html>"),
            super::SeedCounts::default()
        );
        let mut total = super::SeedCounts {
            created: 1,
            updated: 1,
            unchanged: 1,
        };
        total.add(super::SeedCounts {
            created: 2,
            updated: 3,
            unchanged: 4,
        });
        assert_eq!(
            total,
            super::SeedCounts {
                created: 3,
                updated: 4,
                unchanged: 5
            }
        );

        assert!(super::seed_records("not: [a seed").is_none());
        assert!(super::seed_records("records: []\n").is_none());
        let (lookup, records) =
            super::seed_records("lookup_fields: [email]\nrecords: [{email: a}]\n").unwrap();
        assert_eq!((lookup.len(), records.len()), (1, 1));
        let document =
            super::seed_document(&["email"], &[serde_json::json!({"email": "a"})]).unwrap();
        assert_eq!(super::seed_records(&document).unwrap().1.len(), 1);

        let untouched = super::SeedFailure {
            committed: 0,
            error: anyhow::anyhow!("refused"),
        }
        .into_error(0);
        assert!(untouched.downcast_ref::<super::PartialImport>().is_none());
        assert_eq!(exit_code_for(&untouched), ExitCode::from(2));
        let partial = super::SeedFailure {
            committed: 50,
            error: anyhow::anyhow!("refused"),
        }
        .into_error(100);
        assert_eq!(
            partial
                .downcast_ref::<super::PartialImport>()
                .unwrap()
                .committed,
            150
        );
        assert_eq!(
            format!("{partial}"),
            "150 record(s) were committed before the import stopped"
        );
        assert_eq!(exit_code_for(&partial), ExitCode::from(4));

        let refused = |status: Option<u16>| super::SeedRefused {
            status: status.map(|code| reqwest::StatusCode::from_u16(code).unwrap()),
            message: String::new(),
        };
        assert!(refused(None).may_have_committed());
        assert!(refused(Some(503)).may_have_committed());
        assert!(!refused(Some(422)).may_have_committed());
    }

    #[test]
    fn ci_forge_names_its_runner_grant_and_document_door() {
        let lookup = |value: Option<&'static str>| {
            move |name: &str| {
                (name == "FORGEJO_ACTIONS")
                    .then_some(value)
                    .flatten()
                    .map(str::to_string)
            }
        };
        assert_eq!(
            super::CiForge::from_lookup(lookup(Some(" true "))),
            super::CiForge::Forgejo
        );
        assert_eq!(
            super::CiForge::from_lookup(lookup(Some("false"))),
            super::CiForge::GitHub
        );
        assert_eq!(
            super::CiForge::from_lookup(lookup(None)),
            super::CiForge::GitHub
        );
        assert_eq!(super::CiForge::GitHub.name(), "GitHub Actions");
        assert_eq!(super::CiForge::Forgejo.name(), "Forgejo Actions");
        assert_eq!(super::CiForge::GitHub.oidc_grant(), "`id-token: write`");
        assert_eq!(
            super::CiForge::Forgejo.oidc_grant(),
            "`enable-openid-connect: true`"
        );
        assert_eq!(
            super::CiForge::GitHub.document_mint_path(),
            "/auth/ci/document-token"
        );
    }

    #[test]
    fn a_mint_refusal_message_prefers_the_doors_own_message() {
        assert_eq!(
            super::mint_refusal_message(r#"{"error":"forbidden","message":"repo not linked"}"#),
            "repo not linked"
        );
        assert_eq!(
            super::mint_refusal_message("\n<html>proxy timeout</html>\n"),
            "<html>proxy timeout</html>"
        );
        assert_eq!(super::mint_refusal_message(""), "(empty response)");
        assert_eq!(super::MintRefusal.to_string(), "CI token mint was refused");
    }

    #[test]
    fn authenticated_transfers_need_https_unless_the_deployment_is_loopback() {
        for allowed in [
            "https://navigator.example.com",
            "http://localhost:8080",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
        ] {
            assert!(
                super::authenticated_client_builder(allowed).is_ok(),
                "{allowed} is allowed"
            );
        }
        for refused in ["http://navigator.example.com", "ftp://127.0.0.1"] {
            assert_eq!(
                super::authenticated_client_builder(refused)
                    .unwrap_err()
                    .to_string(),
                "authenticated transfers require HTTPS except for a loopback deployment",
                "{refused}"
            );
        }
        assert_eq!(
            super::authenticated_client_builder("not a url")
                .unwrap_err()
                .to_string(),
            "parse authenticated deployment URL"
        );
    }

    #[tokio::test]
    async fn step_and_status_reads_name_a_missing_notation_and_a_server_failure() {
        let server = MockServer::start().await;
        let missing = Uuid::now_v7();
        let broken = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{missing}/step")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{broken}/step")))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({ "error": "walker crashed" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{missing}/review")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/lawyer/notations/{broken}/review")))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let uri = server.uri();

        assert_eq!(
            fetch_step(&client, &uri, "t", missing)
                .await
                .unwrap_err()
                .to_string(),
            format!("no notation {missing} on {uri}")
        );
        let step_failure = fetch_step(&client, &uri, "t", broken)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            step_failure.starts_with("step failed:") && step_failure.contains("walker crashed"),
            "{step_failure}"
        );
        assert_eq!(
            fetch_status(&uri, "t", missing)
                .await
                .unwrap_err()
                .to_string(),
            format!("no notation {missing} on {uri}")
        );
        assert_eq!(
            fetch_status(&uri, "t", broken)
                .await
                .unwrap_err()
                .to_string(),
            "notation status failed: 503 Service Unavailable"
        );
    }

    #[test]
    fn local_pointer_keys_follow_the_new_slugs_extension() {
        assert_eq!(
            super::pointer_candidates("engagement", "engagement-letter.pdf"),
            vec!["engagement".to_string(), "engagement.pdf".to_string()]
        );
        assert_eq!(
            super::pointer_candidates("letter.pdf", "moved.pdf"),
            vec!["letter.pdf".to_string()]
        );
        assert_eq!(
            super::pointer_candidates("letter", "moved"),
            vec!["letter".to_string()]
        );
        assert_eq!(
            super::blob_relative("onboarding/letter.pdf"),
            format!(
                "{}/onboarding/letter.pdf.yaml",
                crate::document_sync::POINTER_DIRECTORY
            )
        );
        assert_eq!(
            super::bytes_relative("letter.pdf"),
            format!("{}/letter.pdf", crate::document_sync::DOCUMENT_DIRECTORY)
        );
        // An unreadable pointer lays the bytes out at the key itself.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            super::laid_out_bytes_relative(dir.path(), "letter.pdf", "blobs/absent.yaml"),
            super::bytes_relative("letter.pdf")
        );
        // With no local pointer there is nothing to refuse.
        super::validate_local_pointer_move(dir.path(), "letter.pdf", "moved.pdf").unwrap();
    }

    #[test]
    fn asset_kinds_and_upload_sizes_are_checked_before_any_request() {
        assert!(super::is_asset_kind("unclassified"));
        assert!(!super::is_asset_kind("not-a-kind"));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("small.txt");
        std::fs::write(&file, b"abc").unwrap();
        super::validate_document_upload_size("unclassified", &file).unwrap();
        assert!(
            super::validate_document_upload_size("unclassified", &dir.path().join("absent"))
                .unwrap_err()
                .to_string()
                .starts_with("read metadata for")
        );
        let cap = store::documents::max_document_upload_bytes("unclassified");
        assert_eq!(
            super::validate_document_upload_size_in_bytes("unclassified", cap + 1)
                .unwrap_err()
                .to_string(),
            store::documents::document_upload_size_message_for_kind("unclassified", cap + 1)
        );
    }

    #[test]
    fn a_step_question_reports_whether_it_offers_candidates() {
        let step: StepResponse = serde_json::from_value(serde_json::json!({
            "question": {"code": "q", "prompt": "Pick", "answer_type": "text"}
        }))
        .unwrap();
        assert!(!step.question.unwrap().has_candidates());
        let step: StepResponse = serde_json::from_value(serde_json::json!({
            "question": {
                "code": "q",
                "prompt": "Pick",
                "answer_type": "entity",
                "candidates": [{"id": Uuid::now_v7(), "name": "Acme"}]
            }
        }))
        .unwrap();
        assert!(step.question.unwrap().has_candidates());
        let done: StepResponse = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(done.question.is_none());
    }
}
