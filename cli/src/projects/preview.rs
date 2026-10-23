//! `navigator project preview` — the matter's declared pages, rendered on
//! `localhost` from the working tree, under a draft banner.
//!
//! An author edits `navigator.yaml`, `drafts/` (or `templates/`),
//! `documents/` pointers, and `notes/` in their editor. This command serves
//! each [Matter Page](../../../docs/glossary/matter-page.md) the checkout's
//! `pages:` declares through the same `webapp::sections` components the
//! deployment's `/app/projects/{code}/{slug}` route renders, so the preview
//! cannot drift from the published page. Rendering is local: nothing is
//! pushed to the host to preview a page.
//!
//! - **Working tree.** Every request reads the checkout. The page polls a
//!   fingerprint of the watched paths and reloads when a save changes it.
//! - **Host, read only.** Section data that lives only in Navigator
//!   (Deadlines) is read from the manifest's host with the stored `navigator
//!   site login` session. Without one the command refuses before it serves
//!   anything. Every request to the host goes through [`ReadOnlyHost`],
//!   which can only send a `GET`.
//! - **Banner.** A fixed, non-dismissable banner tops every page and lens:
//!   [`BANNER`], linking to the repository's new-pull-request URL and naming
//!   the host and branch. Under it are the offline gate's findings, re-run on
//!   each save, and the lens switcher. The deployed route never carries it.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{Path as PathParam, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use uuid::Uuid;
use webapp::people::ViewerRole;
use webapp::sections::page::{self as matter_page, MatterPageView};
use webapp::sections::{LoadContext, PageLens, SectionRow};

use super::manifest::{self, Manifest};
use crate::remote::{self, MatchingSession};

/// The banner's sentence, on every page and lens of the preview.
pub(crate) const BANNER: &str = "This is a draft. Create a pull request to publish.";

/// The working-tree paths whose edits reload the preview.
const WATCHED: &[&str] = &[manifest::FILE, "drafts", "templates", "blobs", "notes"];

/// The cookie that keeps the chosen lens across tab clicks.
const LENS_COOKIE: &str = "navigator_preview_lens";

/// `navigator project preview`: serve the checkout's pages on `port` (`0`
/// picks a free one) until interrupted.
pub async fn run(port: u16) -> ExitCode {
    match serve(port).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project preview: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn serve(port: u16) -> Result<()> {
    let root = std::env::current_dir().context("reading the current directory")?;
    let parsed = manifest::read(&root).ok_or_else(|| {
        anyhow!(
            "no readable {} here; run `navigator project preview` from a Project checkout root",
            manifest::FILE
        )
    })?;
    let host = manifest::non_empty(parsed.host.as_deref())
        .ok_or_else(|| anyhow!("{} names no project.host to read from", manifest::FILE))?;
    let (base, token) = session_or_refusal(&host, remote::matching_stored_session(&host)?)
        .map_err(|refusal| anyhow!(refusal))?;
    let branch = current_branch(&root).unwrap_or_else(|| "HEAD".to_string());
    let pull_request = origin_url(&root).and_then(|url| new_pull_request_url(&url, &branch));
    let preview = Preview::new(
        root,
        host.clone(),
        ReadOnlyHost::new(base, token),
        branch.clone(),
        pull_request,
    );

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let bound = listener.local_addr().context("reading the bound port")?;
    let url = format!("http://{bound}/");
    eprintln!("==> draft preview of branch `{branch}`, reading section data from {host}");
    eprintln!("    rendered locally; nothing is sent to the host but read-only GETs.");
    eprintln!("==> {url}");
    eprintln!("    Ctrl-C to stop.");
    if crate::notations_preview::open_browser(&url) {
        eprintln!("    opened in your browser.");
    }
    axum::serve(listener, router(preview))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving the preview")
}

/// The stored session for exactly the manifest's host, or the refusal that
/// names the login command. The preview never falls back to a session for
/// some other deployment, and never serves without one.
fn session_or_refusal(host: &str, session: MatchingSession) -> Result<(String, String), String> {
    match session {
        MatchingSession::Valid(base, token) => Ok((base, token)),
        MatchingSession::Absent => Err(format!(
            "not logged in to {host}, so section data cannot be read; run `navigator site login --host {host}` and try again"
        )),
        MatchingSession::Expired => Err(format!(
            "the stored login for {host} has expired; run `navigator site login --host {host}` and try again"
        )),
    }
}

/// The deployment, reachable only through `GET`. This is the preview's one
/// door to the host, so no code path in it can send a write.
#[derive(Clone)]
pub(crate) struct ReadOnlyHost {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl ReadOnlyHost {
    pub(crate) fn new(base: String, token: String) -> Self {
        Self {
            base,
            token,
            client: reqwest::Client::new(),
        }
    }

    async fn get(&self, route: &str) -> Result<serde_json::Value> {
        let response = self
            .client
            .get(format!("{}{route}", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .with_context(|| format!("GET {route}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("GET {route} answered {status}"));
        }
        response
            .json()
            .await
            .with_context(|| format!("parse GET {route}"))
    }
}

/// A Project row from `GET /app/api/projects`.
#[derive(Debug, Deserialize)]
struct LiveProject {
    id: Uuid,
    code: String,
    #[serde(default)]
    name: Option<String>,
}

/// A Deadline as `GET /app/api/projects/{id}/deadlines` serializes it.
#[derive(Debug, Deserialize)]
struct LiveDeadline {
    id: Uuid,
    project_id: Uuid,
    due_at: String,
    #[serde(default)]
    authority_kind: String,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    source_document_id: Option<Uuid>,
    #[serde(default)]
    owner_person_id: Option<Uuid>,
    status: String,
    #[serde(default)]
    calendar_role: String,
    #[serde(default)]
    internal_lead_days: Option<i64>,
    #[serde(default)]
    client_lead_days: Option<i64>,
    #[serde(default)]
    replay_key: Option<String>,
    #[serde(default)]
    inserted_at: String,
    #[serde(default)]
    updated_at: String,
}

impl From<LiveDeadline> for store::project_deadlines::Deadline {
    fn from(row: LiveDeadline) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            due_at: row.due_at,
            authority_kind: row.authority_kind,
            authority: row.authority,
            source: row.source,
            source_document_id: row.source_document_id,
            owner_person_id: row.owner_person_id,
            status: row.status,
            calendar_role: row.calendar_role,
            internal_lead_days: row.internal_lead_days,
            client_lead_days: row.client_lead_days,
            replay_key: row.replay_key,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// What the host holds for the matter, read once per working-tree change.
#[derive(Debug, Default)]
struct HostData {
    matter_name: Option<String>,
    deadlines: Vec<store::project_deadlines::Deadline>,
    /// Set when the host has no live matter for this code yet.
    note: Option<String>,
}

async fn read_host(host: &ReadOnlyHost, code: &str) -> Result<HostData> {
    let projects: Vec<LiveProject> = serde_json::from_value(host.get("/app/api/projects").await?)
        .context("parse GET /app/api/projects")?;
    let Some(project) = projects.into_iter().find(|project| project.code == code) else {
        return Ok(HostData {
            note: Some(
                "The host has no live matter for this Project yet, so every section shows its \
                 empty state."
                    .to_string(),
            ),
            ..HostData::default()
        });
    };
    let deadlines: Vec<LiveDeadline> = serde_json::from_value(
        host.get(&format!("/app/api/projects/{}/deadlines", project.id))
            .await?,
    )
    .context("parse the deadlines")?;
    Ok(HostData {
        matter_name: project.name,
        deadlines: deadlines.into_iter().map(Into::into).collect(),
        note: None,
    })
}

/// One finding shown under the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewFinding {
    warning: bool,
    location: String,
    code: String,
    message: String,
}

/// One `documents/` pointer in the working tree.
#[derive(Debug, Clone)]
struct LocalDocument {
    label: String,
    visibility: String,
}

/// Everything one working-tree state yields, recomputed when it changes.
struct Snapshot {
    fingerprint: String,
    manifest: Result<Manifest, String>,
    findings: Vec<PreviewFinding>,
    documents: Vec<LocalDocument>,
    drafts: usize,
    notes: usize,
    host: Result<HostData, String>,
    read_at: String,
}

/// The preview's shared state.
pub(crate) struct Preview {
    root: PathBuf,
    host_name: String,
    host: ReadOnlyHost,
    branch: String,
    pull_request: Option<String>,
    snapshot: tokio::sync::Mutex<Option<Arc<Snapshot>>>,
}

impl Preview {
    pub(crate) fn new(
        root: PathBuf,
        host_name: String,
        host: ReadOnlyHost,
        branch: String,
        pull_request: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            root,
            host_name,
            host,
            branch,
            pull_request,
            snapshot: tokio::sync::Mutex::new(None),
        })
    }

    /// The snapshot for the working tree as it is now, rebuilt only when a
    /// watched path changed since the last one.
    async fn current(&self) -> Arc<Snapshot> {
        let fingerprint = fingerprint(&self.root);
        let mut slot = self.snapshot.lock().await;
        if let Some(snapshot) = slot.as_ref() {
            if snapshot.fingerprint == fingerprint {
                return Arc::clone(snapshot);
            }
        }
        let snapshot = Arc::new(self.build(fingerprint).await);
        *slot = Some(Arc::clone(&snapshot));
        snapshot
    }

    async fn build(&self, fingerprint: String) -> Snapshot {
        let manifest = std::fs::read_to_string(self.root.join(manifest::FILE))
            .map_err(|error| format!("cannot read {}: {error}", manifest::FILE))
            .and_then(|contents| manifest::parse(&contents));
        let root = self.root.clone();
        let (findings, documents, drafts, notes) = tokio::task::spawn_blocking(move || {
            let (documents, mut pointer_findings) = local_documents(&root);
            let mut findings = offline_findings(&root);
            findings.append(&mut pointer_findings);
            (
                findings,
                documents,
                count_files(&root, &["drafts", "templates"]),
                count_files(&root, &["notes"]),
            )
        })
        .await
        .unwrap_or_default();
        let host = match manifest.as_ref().ok().and_then(|m| m.project.clone()) {
            Some(code) => read_host(&self.host, &code)
                .await
                .map_err(|error| format!("{error:#}")),
            None => Err(format!("{} names no project", manifest::FILE)),
        };
        Snapshot {
            fingerprint,
            manifest,
            findings,
            documents,
            drafts,
            notes,
            host,
            read_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        }
    }
}

/// A fingerprint of every watched path's name, size, and modification
/// time. A save changes it; the page polls it and reloads.
fn fingerprint(root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    for watched in WATCHED {
        for entry in walkdir::WalkDir::new(root.join(watched))
            .sort_by_file_name()
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            entry.path().hash(&mut hasher);
            if let Ok(metadata) = entry.metadata() {
                metadata.len().hash(&mut hasher);
                if let Ok(modified) = metadata.modified() {
                    modified.hash(&mut hasher);
                }
            }
        }
    }
    format!("{:016x}", hasher.finish())
}

fn count_files(root: &Path, dirs: &[&str]) -> usize {
    dirs.iter()
        .map(|dir| {
            walkdir::WalkDir::new(root.join(dir))
                .into_iter()
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .count()
        })
        .sum()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The offline checks `navigator project gate` runs, in-process and without
/// writing: the classified Markdown rules (a fix the gate would make is
/// reported, not applied), the cross-file checks, and the Project-manifest
/// pass, `pages:` included.
fn offline_findings(root: &Path) -> Vec<PreviewFinding> {
    let mut findings = Vec::new();
    let mut violation = |violation: rules::Violation| {
        findings.push(PreviewFinding {
            warning: rules::severity_for_code(violation.code) == rules::Severity::Warning,
            location: format!("{}:{}", relative(root, &violation.path), violation.line),
            code: violation.code.to_string(),
            message: violation.message,
        });
    };
    let filter = crate::ProjectGateMarkdownFilter::new(root);
    let question_codes = rules::canonical_question_codes();
    let mut fixable = Vec::new();
    match crate::fix_directory(
        root,
        &filter,
        |file| rules::navigator_classified_rules_with_codes(file, &question_codes),
        false,
    ) {
        Ok(report) => {
            report.remaining.into_iter().for_each(&mut violation);
            fixable = report.fixed_files;
        }
        Err(error) => violation(rules::Violation {
            code: "F001",
            path: root.to_path_buf(),
            line: 0,
            range: 0..0,
            message: format!("could not read the tree: {error}"),
        }),
    }
    for cross_file in [
        rules::code_uniqueness_violations(root, &filter),
        rules::service_template_violations(root, &filter),
        rules::project_skill_catalog_violations(root, &filter),
    ] {
        cross_file.into_iter().flatten().for_each(&mut violation);
    }
    for path in fixable {
        findings.push(PreviewFinding {
            warning: true,
            location: relative(root, &path),
            code: "F001".to_string(),
            message: "not formatted; `navigator project gate` fixes it".to_string(),
        });
    }
    for finding in manifest::lint(root) {
        findings.push(PreviewFinding {
            warning: finding.warning,
            location: format!("{}:{}", relative(root, &finding.path), finding.line),
            code: finding.code.to_string(),
            message: finding.message,
        });
    }
    findings
}

/// Each `blobs/` pointer's path and visibility. A pointer that does not
/// parse is a `Y003` finding instead.
fn local_documents(root: &Path) -> (Vec<LocalDocument>, Vec<PreviewFinding>) {
    let mut documents = Vec::new();
    let mut findings = Vec::new();
    let base = root.join(crate::document_sync::POINTER_DIRECTORY);
    for entry in walkdir::WalkDir::new(&base)
        .sort_by_file_name()
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if !crate::document_sync::is_pointer_path(path) {
            continue;
        }
        let parsed = std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|raw| store::document_pointers::DocumentPointer::from_yaml(&raw));
        match parsed {
            Ok(pointer) => {
                let under = relative(&base, path);
                let label = crate::document_sync::strip_pointer_extension(&under)
                    .unwrap_or(&under)
                    .to_string();
                documents.push(LocalDocument {
                    label,
                    visibility: pointer.visibility,
                });
            }
            Err(error) => findings.push(PreviewFinding {
                warning: false,
                location: relative(root, path),
                code: "Y003".to_string(),
                message: format!("{error:#}"),
            }),
        }
    }
    (documents, findings)
}

fn document_rows(documents: &[LocalDocument], lens: PageLens) -> Vec<SectionRow> {
    match lens {
        PageLens::Clerk => Vec::new(),
        PageLens::Client | PageLens::Lawyer => documents
            .iter()
            .filter(|document| {
                lens == PageLens::Lawyer
                    || document.visibility == store::documents::visibility::CLIENT
            })
            .map(|document| SectionRow {
                label: document.label.clone(),
                detail: String::new(),
                status: String::new(),
            })
            .collect(),
    }
}

fn parse_lens(value: &str) -> Option<PageLens> {
    match value {
        "lawyer" => Some(PageLens::Lawyer),
        "client" => Some(PageLens::Client),
        "clerk" => Some(PageLens::Clerk),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct LensQuery {
    lens: Option<String>,
}

/// The lens a request asks for: `?lens=` first, then the cookie, then
/// lawyer. Returns whether the query chose it, so the response can keep it.
fn requested_lens(query: &LensQuery, headers: &HeaderMap) -> (PageLens, bool) {
    if let Some(lens) = query.lens.as_deref().and_then(parse_lens) {
        return (lens, true);
    }
    let from_cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == LENS_COOKIE)
        .and_then(|(_, value)| parse_lens(value));
    (from_cookie.unwrap_or(PageLens::Lawyer), false)
}

fn role_for(lens: PageLens) -> ViewerRole {
    match lens {
        PageLens::Lawyer => ViewerRole::Lawyer,
        PageLens::Client => ViewerRole::Client,
        PageLens::Clerk => ViewerRole::Clerk,
    }
}

/// The preview's routes: each page at the deployment's own path, the
/// matter overview, the reload fingerprint, and the theme stylesheet.
pub(crate) fn router(preview: Arc<Preview>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/app/projects/{code}", get(overview))
        .route("/app/projects/{code}/{slug}", get(page))
        .route("/__preview/version", get(version))
        .route("/public/css/{*path}", get(stylesheet))
        .fallback(not_found)
        .with_state(preview)
}

async fn version(State(preview): State<Arc<Preview>>) -> String {
    fingerprint(&preview.root)
}

async fn stylesheet(PathParam(path): PathParam<String>) -> Response {
    match crate::notations_preview::PUBLIC_CSS.get_file(&path) {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            file.contents().to_vec(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no such stylesheet").into_response(),
    }
}

async fn home(State(preview): State<Arc<Preview>>) -> Response {
    let snapshot = preview.current().await;
    match snapshot
        .manifest
        .as_ref()
        .ok()
        .and_then(|m| m.project.clone())
    {
        Some(code) => Redirect::temporary(&format!("/app/projects/{code}")).into_response(),
        None => respond(
            &preview,
            &snapshot,
            "/",
            PageLens::Lawyer,
            false,
            StatusCode::OK,
            "<main id=\"matter-page\" class=\"nav-theme\"><h1>No Project</h1>\
             <p>Fix the manifest findings above to see the pages.</p></main>",
        ),
    }
}

async fn overview(
    State(preview): State<Arc<Preview>>,
    PathParam(code): PathParam<String>,
    Query(query): Query<LensQuery>,
    headers: HeaderMap,
) -> Response {
    let snapshot = preview.current().await;
    let (lens, chosen) = requested_lens(&query, &headers);
    let path = format!("/app/projects/{code}");
    let pages = snapshot
        .manifest
        .as_ref()
        .map(|m| m.pages.clone())
        .unwrap_or_default();
    let mut body = format!(
        "<main id=\"matter-page\" class=\"nav-theme portal-detail\" data-lens=\"{}\"><h1>{}</h1>",
        lens.as_str(),
        escape(&code)
    );
    let tabs = matter_page::page_tabs(&code, &pages, lens);
    if tabs.len() < 2 {
        let _ = write!(
            body,
            "<p>No page in <code>pages:</code> has a <code>{}</code> face.</p>",
            lens.as_str()
        );
    } else {
        body.push_str("<ul>");
        for tab in tabs.iter().skip(1) {
            let _ = write!(
                body,
                "<li><a class=\"nav-link\" href=\"{}\">{}</a></li>",
                escape(&tab.href),
                escape(&tab.label)
            );
        }
        body.push_str("</ul>");
    }
    body.push_str("</main>");
    respond(
        &preview,
        &snapshot,
        &path,
        lens,
        chosen,
        StatusCode::OK,
        &body,
    )
}

async fn page(
    State(preview): State<Arc<Preview>>,
    PathParam((code, slug)): PathParam<(String, String)>,
    Query(query): Query<LensQuery>,
    headers: HeaderMap,
) -> Response {
    let snapshot = preview.current().await;
    let (lens, chosen) = requested_lens(&query, &headers);
    let path = format!("/app/projects/{code}/{slug}");
    let declared = snapshot
        .manifest
        .as_ref()
        .ok()
        .filter(|m| m.project.as_deref() == Some(code.as_str()))
        .map(|m| m.pages.clone());
    let Some(pages) = declared else {
        return missing(&preview, &snapshot, &path, lens, chosen);
    };
    let Some((entry, sections)) = matter_page::page_sections(&pages, &slug, lens) else {
        return missing(&preview, &snapshot, &path, lens, chosen);
    };
    let host = snapshot.host.as_ref().ok();
    let mut cx = LoadContext::empty(lens);
    cx.slug.clone_from(&slug);
    cx.synced_at = format!("{} (working tree)", snapshot.read_at);
    cx.deadlines = matter_page::deadline_section_rows(
        host.map_or(&[][..], |data| data.deadlines.as_slice()),
        chrono::Utc::now(),
        lens,
    );
    cx.documents = document_rows(&snapshot.documents, lens);
    let view = MatterPageView {
        tabs: matter_page::page_tabs(&code, &pages, lens),
        matter_name: host
            .and_then(|data| data.matter_name.clone())
            .unwrap_or_else(|| code.clone()),
        code,
        slug,
        title: entry.title,
        lens,
        sections: sections
            .into_iter()
            .map(|section| webapp::sections::load(section, &cx))
            .collect(),
        role: role_for(lens),
        logo: None,
        tokens_href: String::new(),
    };
    let body = matter_page::render_matter_page_body(view);
    respond(
        &preview,
        &snapshot,
        &path,
        lens,
        chosen,
        StatusCode::OK,
        &body,
    )
}

fn missing(
    preview: &Preview,
    snapshot: &Snapshot,
    path: &str,
    lens: PageLens,
    chosen: bool,
) -> Response {
    let body = format!(
        "<main id=\"matter-page\" class=\"nav-theme\"><h1>Not found</h1>\
         <p><code>pages:</code> declares no page here with a <code>{}</code> face.</p></main>",
        lens.as_str()
    );
    respond(
        preview,
        snapshot,
        path,
        lens,
        chosen,
        StatusCode::NOT_FOUND,
        &body,
    )
}

async fn not_found(
    State(preview): State<Arc<Preview>>,
    uri: axum::http::Uri,
    Query(query): Query<LensQuery>,
    headers: HeaderMap,
) -> Response {
    let snapshot = preview.current().await;
    let (lens, chosen) = requested_lens(&query, &headers);
    missing(&preview, &snapshot, uri.path(), lens, chosen)
}

/// A full page: the banner, then `body`. Remembers a lens the query chose.
fn respond(
    preview: &Preview,
    snapshot: &Snapshot,
    path: &str,
    lens: PageLens,
    chosen: bool,
    status: StatusCode,
    body: &str,
) -> Response {
    let html = document(preview, snapshot, path, lens, body);
    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response();
    if chosen {
        if let Ok(value) =
            format!("{LENS_COOKIE}={}; Path=/; SameSite=Strict", lens.as_str()).parse()
        {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    response
}

/// The repository's new-pull-request URL for `branch`, from its `origin`
/// remote in any spelling `git` accepts.
fn new_pull_request_url(remote: &str, branch: &str) -> Option<String> {
    let (host, path) = crate::devx::github_setup::split_remote(remote.trim())?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("ssh.").unwrap_or(&host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    if host.is_empty() || path.split('/').count() != 2 {
        return None;
    }
    Some(format!("https://{host}/{path}/compare/{branch}?expand=1"))
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_branch(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
}

fn origin_url(root: &Path) -> Option<String> {
    git(root, &["remote", "get-url", "origin"])
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// The banner: the draft sentence and its pull-request link, the host and
/// branch, the lens switcher, what was read, and the offline gate's
/// findings. It has no control that hides it.
fn banner(preview: &Preview, snapshot: &Snapshot, path: &str, lens: PageLens) -> String {
    let mut html = String::from(
        "<div id=\"draft-banner\" class=\"draft-banner\" role=\"region\" aria-label=\"Draft preview\">",
    );
    let _ = write!(
        html,
        "<p class=\"draft-banner__headline\"><strong>{}</strong> ",
        escape(BANNER)
    );
    match &preview.pull_request {
        Some(url) => {
            let _ = write!(
                html,
                "<a class=\"draft-banner__pr\" href=\"{}\">Open a pull request</a>",
                escape(url)
            );
        }
        None => html.push_str("This checkout has no <code>origin</code> remote to open one on."),
    }
    html.push_str("</p>");
    let _ = write!(
        html,
        "<p>Branch <code>{}</code>, section data read from <code>{}</code>.</p>",
        escape(&preview.branch),
        escape(&preview.host_name)
    );
    html.push_str("<nav class=\"draft-banner__lenses\" aria-label=\"Lens\">Lens:");
    for option in [PageLens::Lawyer, PageLens::Client, PageLens::Clerk] {
        let current = if option == lens {
            " aria-current=\"true\""
        } else {
            ""
        };
        let _ = write!(
            html,
            " <a href=\"{}?lens={name}\"{current}>{name}</a>",
            escape(path),
            name = option.as_str()
        );
    }
    html.push_str("</nav>");
    let _ = write!(
        html,
        "<p class=\"draft-banner__read\">Read from the working tree: {}, {} draft(s), {} \
         document pointer(s), {} note(s).</p>",
        manifest::FILE,
        snapshot.drafts,
        snapshot.documents.len(),
        snapshot.notes
    );
    if let Err(error) = &snapshot.manifest {
        let _ = write!(
            html,
            "<p class=\"draft-banner__error\">{}</p>",
            escape(error)
        );
    }
    match &snapshot.host {
        Ok(data) => {
            if let Some(note) = &data.note {
                let _ = write!(html, "<p>{}</p>", escape(note));
            }
        }
        Err(error) => {
            let _ = write!(
                html,
                "<p class=\"draft-banner__error\">Could not read section data from the host: {}</p>",
                escape(error)
            );
        }
    }
    html.push_str("<section class=\"draft-banner__findings\" aria-label=\"Gate findings\">");
    if snapshot.findings.is_empty() {
        html.push_str("<p>The offline gate found nothing.</p>");
    } else {
        let _ = write!(
            html,
            "<p>The offline gate found {}:</p><ul>",
            snapshot.findings.len()
        );
        for finding in &snapshot.findings {
            let _ = write!(
                html,
                "<li data-code=\"{code}\">{severity} <code>{location}</code> {code}: \
                 {message}</li>",
                severity = if finding.warning {
                    "warning:"
                } else {
                    "error:"
                },
                location = escape(&finding.location),
                code = escape(&finding.code),
                message = escape(&finding.message),
            );
        }
        html.push_str("</ul>");
    }
    html.push_str("</section></div>");
    html
}

const BANNER_STYLE: &str = "\
.draft-banner{position:fixed;top:0;left:0;right:0;z-index:2147483647;max-height:45vh;overflow:auto;\
padding:.5rem 1rem;background:#fff4ce;color:#1f1f1f;border-bottom:2px solid #8a6d00;font:14px/1.4 system-ui,sans-serif}\
.draft-banner p{margin:.25rem 0}.draft-banner a{color:#00518f}.draft-banner__error{color:#8c1d18}\
.draft-banner__lenses a[aria-current]{font-weight:700;text-decoration:none}\
.draft-banner__findings ul{margin:.25rem 0;padding-left:1.25rem}";

/// Keeps the page below the fixed banner, and reloads it when a watched
/// path changes.
fn script(fingerprint: &str) -> String {
    format!(
        "(function(){{var b=document.getElementById('draft-banner');\
function pad(){{document.body.style.paddingTop=b.offsetHeight+'px';}}\
pad();window.addEventListener('resize',pad);var v='{fingerprint}';\
setInterval(function(){{fetch('/__preview/version',{{cache:'no-store'}})\
.then(function(r){{return r.text();}}).then(function(t){{if(t!==v){{location.reload();}}}})\
.catch(function(){{}});}},1000);}})();"
    )
}

fn document(
    preview: &Preview,
    snapshot: &Snapshot,
    path: &str,
    lens: PageLens,
    body: &str,
) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>Draft preview</title>\
<link rel=\"stylesheet\" href=\"{theme}\"><style>{BANNER_STYLE}</style></head>\
<body>{banner}{body}<script>{script}</script></body></html>",
        theme = webapp::components::THEME_STYLESHEET_HREF,
        banner = banner(preview, snapshot, path, lens),
        script = script(&snapshot.fingerprint),
    )
}

#[cfg(test)]
mod tests {
    use super::{new_pull_request_url, router, session_or_refusal, Preview, ReadOnlyHost, BANNER};
    use crate::remote::MatchingSession;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use std::path::Path;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROJECT_ID: &str = "00000000-0000-0000-0000-0000000000b2";

    fn manifest(title: &str) -> String {
        [
            "version: 26.10.6",
            "project:",
            "  host: h.example.com",
            "  name: acme",
            "  lawyer_dri: lawyer@example.com",
            "  client_dri: client@example.com",
            "  private_slack_channel: https://slack.example.com/private",
            "  private_notion_page: https://notion.example.com/private",
            "pages:",
            "  - slug: timeline",
            "    kind: docket_deadline_board",
            &format!("    title: {title}"),
            "    lenses:",
            "      lawyer: [deadline_board, key_dates_strip, boundary_note, provenance_statement]",
            "      client: [key_dates_strip, boundary_note, provenance_statement]",
            "  - slug: files",
            "    kind: deliverable_package",
            "    title: Files",
            "    lenses:",
            "      lawyer: [package_manifest, download_list, boundary_note, provenance_statement]",
            "      client: [package_manifest, download_list, boundary_note, provenance_statement]",
            "  - slug: billing",
            "    kind: engagement_billing_records",
            "    title: Billing",
            "    lenses:",
            "      lawyer: [engagement_letter, invoice, prebill_review, boundary_note, \
             provenance_statement]",
            "",
        ]
        .join("\n")
    }

    /// A synthetic Project checkout with one shared and one firm-only
    /// document pointer and one note.
    fn checkout(title: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("navigator.yaml"), manifest(title)).unwrap();
        std::fs::write(root.join("README.md"), "# Synthetic matter\n").unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/call.md"), "Synthetic call note.\n").unwrap();
        std::fs::create_dir_all(root.join("documents/filed")).unwrap();
        std::fs::create_dir_all(root.join("blobs/filed")).unwrap();
        for (name, visibility) in [("shared.pdf", "client"), ("internal.pdf", "internal")] {
            std::fs::write(
                root.join(format!("blobs/filed/{name}.yaml")),
                format!(
                    "kind: filing\nvisibility: {visibility}\ncurrent_version:\n  version: 1\n  \
                     asset_id: 00000000-0000-0000-0000-0000000000c3\n  \
                     created_at: \"2026-09-17T13:23:46Z\"\n  sha256: {}\n  size_bytes: 1\n",
                    "a".repeat(64)
                ),
            )
            .unwrap();
        }
        dir
    }

    /// A fake host that answers the two reads and refuses anything else.
    async fn fake_host() -> MockServer {
        let host = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": PROJECT_ID, "code": "acme", "name": "Synthetic v. Example" }
            ])))
            .mount(&host)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{PROJECT_ID}/deadlines")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "00000000-0000-0000-0000-0000000000d4",
                    "project_id": PROJECT_ID,
                    "due_at": "2099-11-02T17:00:00-08:00",
                    "authority_kind": "court_order",
                    "authority": "Order setting briefing schedule",
                    "source": "lawyer",
                    "status": "open",
                    "calendar_role": "firm",
                    "internal_lead_days": 7,
                    "client_lead_days": null
                }])),
            )
            .mount(&host)
            .await;
        host
    }

    fn preview(root: &Path, host: &MockServer) -> axum::Router {
        router(Preview::new(
            root.to_path_buf(),
            "h.example.com".to_string(),
            ReadOnlyHost::new(host.uri(), "local-token".to_string()),
            "draft-timeline".to_string(),
            new_pull_request_url("git@github.com:org/acme.git", "draft-timeline"),
        ))
    }

    async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String, Option<String>) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .map(|value| value.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap(), cookie)
    }

    async fn assert_only_reads(host: &MockServer) {
        let requests = host.received_requests().await.unwrap();
        assert!(!requests.is_empty(), "the preview read the host");
        for request in requests {
            assert_eq!(request.method.as_str(), "GET", "{}", request.url);
        }
    }

    #[test]
    fn an_unauthenticated_preview_fails_closed_with_the_login_command() {
        for session in [MatchingSession::Absent, MatchingSession::Expired] {
            let refusal = session_or_refusal("h.example.com", session).unwrap_err();
            assert!(
                refusal.contains("navigator site login --host h.example.com"),
                "{refusal}"
            );
        }
        let (base, token) = session_or_refusal(
            "h.example.com",
            MatchingSession::Valid("https://h.example.com".into(), "t".into()),
        )
        .unwrap();
        assert_eq!(
            (base.as_str(), token.as_str()),
            ("https://h.example.com", "t")
        );
    }

    #[test]
    fn the_new_pull_request_url_comes_from_any_origin_spelling() {
        for remote in [
            "git@github.com:org/acme.git",
            "https://github.com/org/acme",
            "ssh://git@ssh.github.com:443/org/acme.git",
        ] {
            assert_eq!(
                new_pull_request_url(remote, "topic").as_deref(),
                Some("https://github.com/org/acme/compare/topic?expand=1"),
                "{remote}"
            );
        }
        assert_eq!(new_pull_request_url("not a remote", "topic"), None);
    }

    /// ENG-1077: an edit to `navigator.yaml` changes the next response, and
    /// the fingerprint the page polls changes with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn editing_navigator_yaml_changes_the_next_response() {
        let host = fake_host().await;
        let dir = checkout("Case timeline");
        let app = preview(dir.path(), &host);

        let (status, html, _) = get(&app, "/app/projects/acme/timeline").await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(html.contains("Case timeline"), "{html}");
        assert!(html.contains("Order setting briefing schedule"), "{html}");
        assert!(html.contains("Synthetic v. Example"), "{html}");
        let (_, before, _) = get(&app, "/__preview/version").await;

        std::fs::write(
            dir.path().join("navigator.yaml"),
            manifest("Docket and deadlines"),
        )
        .unwrap();
        let (_, html, _) = get(&app, "/app/projects/acme/timeline").await;
        assert!(html.contains("Docket and deadlines"), "{html}");
        assert!(!html.contains("Case timeline"), "{html}");
        let (_, after, _) = get(&app, "/__preview/version").await;
        assert_ne!(before, after);
        assert_only_reads(&host).await;
    }

    /// ENG-1077: the banner, its pull-request link, the host, and the
    /// branch are on every route and lens, with nothing that dismisses it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_banner_is_on_every_route_and_lens() {
        let host = fake_host().await;
        let dir = checkout("Case timeline");
        let app = preview(dir.path(), &host);
        for lens in ["lawyer", "client", "clerk"] {
            for route in [
                "/app/projects/acme/timeline",
                "/app/projects/acme/billing",
                "/app/projects/acme",
                "/no/such/page",
            ] {
                let (_, html, _) = get(&app, &format!("{route}?lens={lens}")).await;
                assert!(html.contains(BANNER), "{route} {lens}: {html}");
                assert!(
                    html.contains("https://github.com/org/acme/compare/draft-timeline?expand=1"),
                    "{route} {lens}"
                );
                assert!(html.contains("h.example.com"), "{route} {lens}");
                assert!(html.contains("draft-timeline"), "{route} {lens}");
                let banner = &html[html.find("id=\"draft-banner\"").unwrap()..];
                let banner = &banner[..banner.find("</div>").unwrap()];
                assert!(!banner.contains("<button"), "{route} {lens}: {banner}");
                assert!(!banner.to_lowercase().contains("dismiss"), "{banner}");
            }
        }
        assert_only_reads(&host).await;
    }

    /// The deployed route renders the same body with no banner at all.
    #[test]
    fn the_deployed_page_carries_no_banner() {
        let html = webapp::sections::page::render_matter_page_body(
            webapp::sections::page::MatterPageView {
                code: "acme".into(),
                matter_name: "Synthetic v. Example".into(),
                slug: "timeline".into(),
                title: "Case timeline".into(),
                lens: webapp::sections::PageLens::Lawyer,
                sections: webapp::sections::SectionView::empty_set(),
                tabs: Vec::new(),
                role: webapp::people::ViewerRole::Lawyer,
                logo: None,
                tokens_href: String::new(),
            },
        );
        assert!(html.contains("Case timeline"), "{html}");
        assert!(!html.contains(BANNER), "{html}");
        assert!(!html.contains("draft-banner"), "{html}");
    }

    /// The lens switcher shows each face: a client sees the key-dates strip
    /// and only client-visible files, a clerk sees no deadlines, and a page
    /// with no client face is not found for a client. The chosen lens is
    /// remembered for the tab links.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_lens_switcher_renders_each_face() {
        let host = fake_host().await;
        let dir = checkout("Case timeline");
        let app = preview(dir.path(), &host);

        let (_, lawyer, _) = get(&app, "/app/projects/acme/timeline").await;
        assert!(lawyer.contains("data-lens=\"lawyer\""), "{lawyer}");
        assert!(lawyer.contains("data-section=\"deadline_board\""));
        let (_, files, _) = get(&app, "/app/projects/acme/files").await;
        assert!(files.contains("filed/internal.pdf"), "{files}");
        assert!(files.contains("filed/shared.pdf"), "{files}");

        let (_, client, cookie) = get(&app, "/app/projects/acme/timeline?lens=client").await;
        assert!(client.contains("data-lens=\"client\""), "{client}");
        assert!(client.contains("data-section=\"key_dates_strip\""));
        assert!(!client.contains("data-section=\"deadline_board\""));
        assert!(
            !client.contains("Order setting briefing schedule"),
            "a deadline with no client lead stays off the client face"
        );
        assert!(cookie.unwrap().contains("navigator_preview_lens=client"));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/app/projects/acme/files")
                    .header(header::COOKIE, "navigator_preview_lens=client")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let files = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            files.contains("data-lens=\"client\""),
            "the cookie keeps the lens"
        );
        assert!(files.contains("filed/shared.pdf"), "{files}");
        assert!(!files.contains("filed/internal.pdf"), "{files}");

        let (_, clerk, _) = get(&app, "/app/projects/acme/timeline?lens=clerk").await;
        assert!(clerk.contains("Not found"), "no clerk face: {clerk}");

        let (status, _, _) = get(&app, "/app/projects/acme/billing?lens=client").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_only_reads(&host).await;
    }

    /// The offline gate runs on each save and its findings sit under the
    /// banner; fixing the file clears them. The preview writes nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn gate_findings_follow_each_save() {
        let host = fake_host().await;
        let dir = checkout("Case timeline");
        let app = preview(dir.path(), &host);
        let (_, html, _) = get(&app, "/app/projects/acme/timeline").await;
        assert!(html.contains("The offline gate found nothing."), "{html}");

        let broken = manifest("Case timeline").replace("slug: timeline", "slug: Bad_Slug");
        std::fs::write(dir.path().join("navigator.yaml"), &broken).unwrap();
        let (_, html, _) = get(&app, "/app/projects/acme").await;
        assert!(html.contains("data-code=\"Y021\""), "{html}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("navigator.yaml")).unwrap(),
            broken,
            "the preview never rewrites the working tree"
        );

        std::fs::write(dir.path().join("navigator.yaml"), manifest("Case timeline")).unwrap();
        let (_, html, _) = get(&app, "/app/projects/acme/timeline").await;
        assert!(!html.contains("data-code=\"Y021\""), "{html}");
        assert_only_reads(&host).await;
    }
}
