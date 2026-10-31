//! The live-status half of `navigator project gate`.
//!
//! Layout, origin, and every content rule run offline in the gate itself. This
//! module checks whether `navigator.yaml` agrees with the live row and its
//! required coordinates. Local runs use the stored `navigator site login`
//! bearer; CI exchanges its GitHub or Forgejo Actions OIDC token on approved
//! deployment refs (`remote::CiForge`). Both paths are
//! read-only, and an unavailable or refused read fails as inconclusive.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::manifest::{self, Manifest};
use crate::remote;

/// The one-line refusal for `--ci` outside a CI runner (ENG-951), or `None`
/// when the flag may proceed. `ci_env` is the value of the `CI` variable; unset
/// and empty both mean "not a runner", the way CI systems define the variable.
pub(crate) fn ci_flag_refusal(ci: bool, ci_env: Option<&str>) -> Option<&'static str> {
    (ci && ci_env.is_none_or(|value| value.trim().is_empty())).then_some(
        "error: --ci is for CI runners (CI is not set). Run `navigator project gate` locally; log in with `navigator site login --host <host>` for the live checks.",
    )
}

/// Check a Project's live status and required coordinates through an
/// authorized local or CI session.
pub(crate) async fn live_status(dir: &Path, ci: bool) -> ExitCode {
    let host = match manifest_host(dir) {
        Ok(host) => host,
        Err(reason) => {
            eprintln!("navigator: live Project check is inconclusive: {reason}");
            return ExitCode::from(1);
        }
    };
    let credentials = if ci {
        if !can_mint_ci_session() || !remote::ci_oidc_available() {
            eprintln!("navigator: live Project check is inconclusive: this CI event/ref cannot mint an authorized session");
            return ExitCode::from(1);
        }
        remote::resolve_ci_document(&host).await
    } else {
        remote::resolve(Some(&host))
    };
    let (base, token) = match credentials {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("navigator: live Project check is inconclusive: {error:#}");
            return ExitCode::from(1);
        }
    };
    let outcome = match check_live_with_token(dir, &base, &token).await {
        Ok(()) => match check_live_handles(dir, &base, &token).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("navigator: live Project coordinates check is inconclusive or disagrees: {error:#}");
                ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("navigator: live Project check is inconclusive or disagrees: {error:#}");
            ExitCode::from(1)
        }
    };
    // `Y023` is a warning: it prints, and never changes the outcome above.
    for finding in section_data_findings(dir, &base, &token).await {
        println!("{}", finding.render());
    }
    outcome
}

/// One live data source a declared section reads, how the gate asks the
/// deployment for its count, and the command that adds one.
struct SectionSource {
    /// The section names (as `pages:` spells them) that render this source.
    sections: &'static [&'static str],
    /// The glossary noun for one row of it, plural.
    noun: &'static str,
    /// The read-only door under `/app/api/projects/{id}/`.
    path: &'static str,
    /// How many rows the response carries that the section would render.
    /// The command that adds a row.
    fix: &'static str,
}

/// Strict aggregate response: malformed or absent counts are inconclusive.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionCounts {
    docket: u64,
    deadlines: u64,
    documents: u64,
}
impl SectionCounts {
    fn rows(&self, source: &SectionSource) -> u64 {
        match source.path {
            "docket" => self.docket,
            "deadlines" => self.deadlines,
            "documents" => self.documents,
            _ => 0,
        }
    }
}

/// Every section whose data the deployment can be asked about. A section
/// absent here reads no live source the gate can count (`boundary_note`,
/// `provenance_statement`), or one with no read door yet (`discovery_pairs`
/// has no stored discovery requests to ask about).
const SECTION_SOURCES: &[SectionSource] = &[
    SectionSource {
        sections: &["docket_timeline"],
        noun: "Docket Entries",
        path: "docket",
        fix: "navigator project docket add --entry-number … --kind … --title …",
    },
    SectionSource {
        sections: &["deadline_board", "key_dates_strip"],
        noun: "open Deadlines",
        path: "deadlines",
        fix: "navigator project deadline create --due-at … --authority-kind … --calendar-role …",
    },
    SectionSource {
        sections: &["download_list"],
        noun: "Documents",
        path: "documents",
        fix: "navigator project sync",
    },
];

/// One `Y023` warning, located at the page entry that declares the section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionDataFinding {
    pub(crate) line: usize,
    pub(crate) message: String,
}

impl SectionDataFinding {
    /// `Y023` never fails the gate: a brand-new matter legitimately has an
    /// empty timeline, and every section renders an empty state.
    pub(crate) const SEVERITY: rules::Severity = rules::Severity::Warning;

    fn render(&self) -> String {
        crate::diagnostic_line(
            Self::SEVERITY,
            &format!("{}:{}", manifest::FILE, self.line),
            Some(manifest::SECTION_DATA_CODE),
            &self.message,
        )
    }
}

/// `Y023`: for each section `pages:` declares, ask the deployment whether
/// the data source it reads has rows, and warn for each that has none. The
/// aggregate is asked once, with a read-only `GET`. A refused or failed read is
/// reported as an inconclusive warning, never a pass and never an error. A
/// manifest that declares `no_live_row`, or whose Project the session cannot
/// see, has no live matter to ask about; the row check reports that.
pub(crate) async fn section_data_findings(
    dir: &Path,
    base: &str,
    token: &str,
) -> Vec<SectionDataFinding> {
    let Ok(contents) = std::fs::read_to_string(dir.join(manifest::FILE)) else {
        return Vec::new();
    };
    let Ok(parsed) = manifest::parse(&contents) else {
        return Vec::new();
    };
    let Some(code) = parsed.project.as_deref() else {
        return Vec::new();
    };
    if parsed.pages.is_empty() || rowless_reason(&parsed).is_some() {
        return Vec::new();
    }
    let declared = rules::pages::declared_pages(&contents);
    let line_of = |slug: &str, section: &str| {
        declared
            .iter()
            .find(|page| page.slug == slug)
            .map_or(1, |page| section_line(&contents, page, section))
    };
    let mut findings = Vec::new();
    let mut project_id = None;
    let mut counts = None;
    for source in SECTION_SOURCES {
        let uses: Vec<(&str, &str)> = parsed
            .pages
            .iter()
            .flat_map(|page| {
                let mut names: Vec<&str> = page
                    .lenses
                    .values()
                    .flatten()
                    .map(String::as_str)
                    .filter(|name| source.sections.contains(name))
                    .collect();
                names.sort_unstable();
                names.dedup();
                names.into_iter().map(|name| (page.slug.as_str(), name))
            })
            .collect();
        let Some(&(first_slug, first_section)) = uses.first() else {
            continue;
        };
        let id = match project_id {
            Some(id) => id,
            None => match super::surfaces::resolve_project_id(base, token, code).await {
                Ok(id) => *project_id.insert(id),
                Err(_) => return findings,
            },
        };
        if counts.is_none() {
            counts = Some(live_counts(base, token, id).await);
        }
        let Some(result) = &counts else {
            continue;
        };
        match result.as_ref().map(|counts| counts.rows(source)) {
            Ok(0) => {
                for (slug, section) in uses {
                    findings.push(SectionDataFinding {
                        line: line_of(slug, section),
                        message: format!(
                            "page `{slug}` declares `{section}`, but the live matter has no {}. \
                             The section renders its empty state; add one with `{}`",
                            source.noun, source.fix
                        ),
                    });
                }
            }
            Ok(_) => {}
            Err(error) => findings.push(SectionDataFinding {
                line: line_of(first_slug, first_section),
                message: format!(
                    "could not ask the deployment whether the matter has {} for `{first_section}`; \
                     inconclusive: {error:#}",
                    source.noun
                ),
            }),
        }
    }
    findings
}

/// The section names whose live source holds at least one row, asked with
/// the same read-only aggregate `GET` that `Y023` sends. A refused or failed read
/// contributes nothing, so a caller treats "unknown" as "no rows".
pub(crate) async fn populated_sections(
    base: &str,
    token: &str,
    project_id: uuid::Uuid,
) -> Vec<&'static str> {
    let Ok(counts) = live_counts(base, token, project_id).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for source in SECTION_SOURCES {
        if counts.rows(source) > 0 {
            out.extend_from_slice(source.sections);
        }
    }
    out
}

/// The line inside `page` naming `section` as a whole word, so
/// `deadline_board` finds the lens line rather than `kind:
/// docket_deadline_board`. Falls back to the entry's first line.
fn section_line(contents: &str, page: &rules::pages::DeclaredPage, section: &str) -> usize {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    contents
        .lines()
        .enumerate()
        .skip(page.line.saturating_sub(1))
        .take(page.end_line.saturating_sub(page.line))
        .find(|(_, line)| {
            line.match_indices(section).any(|(start, _)| {
                let before = line.as_bytes()[..start].last().copied();
                let after = line.as_bytes().get(start + section.len()).copied();
                !before.is_some_and(is_word) && !after.is_some_and(is_word)
            })
        })
        .map_or(page.line, |(index, _)| index + 1)
}

/// Content-free cardinalities for the live matter, read with one `GET`.
async fn live_counts(base: &str, token: &str, project_id: uuid::Uuid) -> Result<SectionCounts> {
    let route = format!("/app/api/projects/{project_id}/section-counts");
    let response = reqwest::Client::new()
        .get(format!("{base}{route}"))
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {route}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("GET {route} answered {status}"));
    }
    let body: SectionCounts = response
        .json()
        .await
        .with_context(|| format!("parse GET {route}"))?;
    Ok(body)
}

async fn check_live_handles(dir: &Path, base: &str, token: &str) -> Result<()> {
    let manifest = manifest::read(dir).ok_or_else(|| anyhow!("cannot parse navigator.yaml"))?;
    let project_code = manifest
        .project
        .as_deref()
        .ok_or_else(|| anyhow!("navigator.yaml has no project.name"))?;
    let projects = super::surfaces::list_visible_projects(base, token).await?;
    let project_id = projects
        .into_iter()
        .find(|project| project.code == project_code)
        .map(|project| project.id)
        .ok_or_else(|| anyhow!("no live row for the declared Project"))?;
    let live = super::surfaces::get_handles(base, token, project_id).await?;
    if super::docket::renumber::has_filings(dir)? {
        let client = remote::DocumentClient::with_credential(
            base.to_string(),
            token.to_string(),
            project_code,
        )
        .await?;
        let integrity = client.integrity(false).await?;
        let metadata: Vec<_> = integrity.assets.iter().map(|asset| serde_json::json!({
            "asset_id": asset.asset_id, "slug": asset.slug, "docket_name_matches": asset.docket_name_matches,
        })).collect();
        super::docket::renumber::validate_checkout(dir, &metadata)?;
    }

    // The records' drift check needs the docket rows, and only a checkout
    // that carries records gives it anything to refuse — the same sparing
    // of live reads the filings check just applied.
    if super::docket::timeline_write::has_records(dir)? {
        let coordinates = super::docket::entries::Coordinates {
            base: base.to_string(),
            token: token.to_string(),
            project_id,
            project_code: project_code.to_string(),
        };
        let rows = super::docket::entries::list_rows(&coordinates).await?;
        super::document_check::check_timeline_records(dir, &rows)?;
    }

    let mut differing = Vec::new();
    if !manifest.lawyer_dri.as_deref().is_some_and(|value| {
        live.lawyer_dri
            .iter()
            .any(|email| email.eq_ignore_ascii_case(value))
    }) {
        differing.push("lawyer_dri");
    }
    if !manifest.client_dri.as_deref().is_some_and(|value| {
        live.client_dri
            .iter()
            .any(|email| email.eq_ignore_ascii_case(value))
    }) {
        differing.push("client_dri");
    }
    if !handle_matches_live(
        manifest.private_slack_channel.as_deref(),
        live.private_slack_channel.as_deref(),
    ) {
        differing.push("private_slack_channel");
    }
    if !handle_matches_live(
        manifest.private_notion_page.as_deref(),
        live.private_notion_page.as_deref(),
    ) {
        differing.push("private_notion_page");
    }
    // Optional: a manifest that names no calendar makes no claim about one.
    if manifest
        .calendar_id
        .as_deref()
        .is_some_and(|value| Some(value) != live.calendar_id.as_deref())
    {
        differing.push("calendar_id");
    }

    if differing.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "navigator.yaml coordinates differ from the live Project: {}",
            differing.join(", ")
        ))
    }
}

/// Whether a manifest's `private_slack_channel`/`private_notion_page` value
/// agrees with the live Project's coordinate.
///
/// `Y016` (`manifest::lint_handle`) lets these two keys carry
/// [`manifest::NOT_PROVISIONED`] instead of a URL, for a Project that
/// genuinely has no live channel or page. That claim only agrees here when
/// the live coordinate is actually absent — this gate verifies it rather
/// than taking the manifest's word for it, so the sentinel can't be used to
/// paper over a coordinate that went live after the manifest was last
/// updated. A missing manifest value never agrees: `Y016` already requires
/// one of a URL or the sentinel, so `None` only reaches here for a manifest
/// this gate does not otherwise trust.
fn handle_matches_live(manifest_value: Option<&str>, live_value: Option<&str>) -> bool {
    match manifest_value {
        None => false,
        Some(value) if manifest::is_not_provisioned(value) => live_value.is_none(),
        Some(value) => Some(value) == live_value,
    }
}

/// The deployment `navigator.yaml` names, which is the only host this gate
/// speaks to. A repository that names none has no live row to check; one
/// whose manifest does not parse is refused with the parser's own reason.
fn manifest_host(dir: &Path) -> Result<String, String> {
    let contents = std::fs::read_to_string(dir.join(manifest::FILE))
        .map_err(|_| "navigator.yaml is missing".to_string())?;
    let parsed = manifest::parse(&contents)?;
    manifest::non_empty(parsed.host.as_deref())
        .ok_or_else(|| "navigator.yaml has no host".to_string())
}

/// Forgejo's runner exports the same `GITHUB_REF` and `GITHUB_EVENT_NAME`
/// compatibility variables, so only the pull-request ref differs by forge.
fn can_mint_ci_session() -> bool {
    let git_ref = std::env::var("GITHUB_REF").unwrap_or_default();
    let event = std::env::var("GITHUB_EVENT_NAME").unwrap_or_default();
    can_mint_ci_session_for(remote::CiForge::detect(), &git_ref, &event)
}

/// `main` pushes and dispatches, plus pull requests on the ref each forge
/// runs them on: GitHub's `refs/pull/<n>/merge`, Forgejo's
/// `refs/pull/<n>/head`. The deployment's mint enforces the same rule.
fn can_mint_ci_session_for(forge: remote::CiForge, git_ref: &str, event: &str) -> bool {
    let pull_suffix = match forge {
        remote::CiForge::GitHub => "/merge",
        remote::CiForge::Forgejo => "/head",
    };
    (git_ref == "refs/heads/main" && matches!(event, "push" | "workflow_dispatch"))
        || (event == "pull_request" && is_pull_request_ref(git_ref, pull_suffix))
}

fn is_pull_request_ref(git_ref: &str, suffix: &str) -> bool {
    let Some(number) = git_ref
        .strip_prefix("refs/pull/")
        .and_then(|reference| reference.strip_suffix(suffix))
    else {
        return false;
    };
    !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && number.parse::<u64>().is_ok_and(|number| number > 0)
}

#[derive(Debug, Deserialize)]
struct LiveMatter {
    code: String,
    status: String,
    #[serde(default)]
    repository_url: Option<String>,
}

#[cfg(test)]
async fn check_live(dir: &Path, host: &str) -> Result<()> {
    let (base, token) = remote::resolve_ci_document(host).await?;
    check_live_with_token(dir, &base, &token).await
}

async fn check_live_with_token(dir: &Path, base: &str, token: &str) -> Result<()> {
    let contents = std::fs::read_to_string(dir.join(manifest::FILE))
        .with_context(|| format!("read {}", dir.join(manifest::FILE).display()))?;
    let parsed = manifest::parse(&contents).map_err(|error| anyhow!("{error}"))?;
    let project = parsed
        .project
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .ok_or_else(|| anyhow!("navigator.yaml has no project:"))?;
    let reason = rowless_reason(&parsed);
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
            "GET /app/api/projects failed: {status}: {}",
            body.lines().next().unwrap_or(&body)
        ));
    }
    let matters: Vec<LiveMatter> =
        serde_json::from_str(&body).context("parse GET /app/api/projects")?;
    let live = matters.iter().find(|matter| matter.code == project);
    disagreement(project, reason, live, this_repository_url(dir).as_deref())
}

fn rowless_reason(manifest: &Manifest) -> Option<&str> {
    match &manifest.no_live_row {
        Some(serde_yaml::Value::String(reason)) => {
            let trimmed = reason.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        _ => None,
    }
}

fn this_repository_url(dir: &Path) -> Option<String> {
    if let Ok(name) = std::env::var("GITHUB_REPOSITORY") {
        let name = name.trim();
        if !name.is_empty() {
            let server =
                std::env::var("GITHUB_SERVER_URL").unwrap_or_else(|_| "https://github.com".into());
            return Some(format!("{}/{name}", server.trim_end_matches('/')));
        }
    }
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8(output.stdout).ok()?;
    Some(remote.trim().to_string())
}

fn disagreement(
    project: &str,
    reason: Option<&str>,
    live: Option<&LiveMatter>,
    this_repo: Option<&str>,
) -> Result<()> {
    match (live, reason) {
        (None, Some(_)) => Ok(()),
        (None, None) => Err(anyhow!(
            "no live row for project `{project}`; declare no_live_row: <reason> if that is intended"
        )),
        (Some(row), Some(_)) if row.status == "open" => Err(anyhow!(
            "live status is `open` but navigator.yaml declares no_live_row"
        )),
        (Some(row), None) if row.status == "closed" || row.status == "archived" => Err(anyhow!(
            "live status is `{}` but navigator.yaml does not declare no_live_row",
            row.status
        )),
        (Some(row), _) => {
            if let (Some(live_url), Some(here)) = (row.repository_url.as_deref(), this_repo) {
                if !repository_urls_agree(live_url, here) {
                    return Err(anyhow!(
                        "repository_url is `{live_url}` but this job is `{here}`"
                    ));
                }
            }
            Ok(())
        }
    }
}

/// Whether two repository URLs name the same repository on the same forge.
///
/// Both sides reduce to `host/owner/repo` through the remote parser `ops
/// github setup` uses (LAW-56), so `https://`, `ssh://…:443` and scp-style
/// spellings of one repository agree. The forge's SSH-over-443 host
/// (`ssh.<host>`) is the same forge as `<host>`; any other host, owner, or
/// repository still mismatches. A string neither form parses compares as
/// written.
fn repository_urls_agree(live: &str, here: &str) -> bool {
    fn normalize(url: &str) -> String {
        let url = url.trim();
        let Some((host, path)) = crate::devx::github_setup::split_remote(url) else {
            return url.trim_end_matches('/').to_ascii_lowercase();
        };
        let host = host.to_ascii_lowercase();
        let host = host.strip_prefix("ssh.").unwrap_or(&host);
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
        format!("{host}/{path}").to_ascii_lowercase()
    }
    normalize(live) == normalize(here)
}

#[cfg(test)]
mod tests {
    use super::{
        can_mint_ci_session_for, check_live_handles, disagreement, is_pull_request_ref,
        manifest_host, LiveMatter,
    };
    use crate::remote::CiForge;
    use axum::body::Body;
    use axum::http::{header, Request};
    use portal::github_oidc::{GitHubActionsClaims, GitHubOidc};
    use std::sync::LazyLock;
    use tower::ServiceExt;
    use wiremock::matchers::{body_json, header as header_matcher, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    fn row(status: &str, url: Option<&str>) -> LiveMatter {
        LiveMatter {
            code: "acme".into(),
            status: status.into(),
            repository_url: url.map(str::to_string),
        }
    }

    #[test]
    fn an_unparseable_manifest_is_named_not_reported_as_hostless() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "version: 26.10.6\nproject:\n  host: h.example.com\n  name: acme\nallowed_links:\n  - https://www.example.com\n",
        )
        .unwrap();
        let error = manifest_host(dir.path()).unwrap_err();
        assert!(error.contains("allowed_links"), "{error}");
        assert!(!error.contains("no host"), "{error}");

        std::fs::write(
            dir.path().join("navigator.yaml"),
            "version: 26.10.6\nproject:\n  host: h.example.com\n  name: acme\nallowed_links:\n  www.example.com: citation\n",
        )
        .unwrap();
        assert_eq!(manifest_host(dir.path()).unwrap(), "h.example.com");
    }

    #[test]
    fn missing_row_without_reason_fails() {
        let error = disagreement("acme", None, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no live row"), "{error}");
        assert!(error.contains("no_live_row"), "{error}");
    }

    #[test]
    fn missing_row_with_reason_passes() {
        disagreement("acme", Some("the matter closed"), None, None).unwrap();
    }

    #[test]
    fn open_row_with_reason_fails() {
        let error = disagreement("acme", Some("closed"), Some(&row("open", None)), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("live status is `open`"), "{error}");
        assert!(error.contains("no_live_row"), "{error}");
    }

    #[test]
    fn closed_row_without_reason_fails() {
        let error = disagreement("acme", None, Some(&row("closed", None)), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("live status is `closed`"), "{error}");
        assert!(error.contains("no_live_row"), "{error}");
    }

    #[test]
    fn archived_row_without_reason_fails() {
        let error = disagreement("acme", None, Some(&row("archived", None)), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("live status is `archived`"), "{error}");
    }

    #[test]
    fn closed_row_with_reason_passes() {
        disagreement(
            "acme",
            Some("the matter closed"),
            Some(&row("closed", None)),
            None,
        )
        .unwrap();
    }

    #[test]
    fn open_row_without_reason_passes() {
        disagreement("acme", None, Some(&row("open", None)), None).unwrap();
    }

    #[test]
    fn repository_url_mismatch_fails() {
        let error = disagreement(
            "acme",
            None,
            Some(&row("open", Some("https://github.com/org/other"))),
            Some("https://github.com/org/acme"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("repository_url"), "{error}");
        assert!(error.contains("org/other"), "{error}");
    }

    #[test]
    fn repository_url_agrees_ignoring_git_suffix() {
        disagreement(
            "acme",
            None,
            Some(&row("open", Some("https://github.com/org/acme.git"))),
            Some("https://github.com/org/acme"),
        )
        .unwrap();
    }

    /// ENG-950: every spelling of one repository agrees with the live row's
    /// `https` URL, including the SSH-over-443 form a fleet checkout uses.
    #[test]
    fn every_spelling_of_the_same_origin_agrees() {
        let live = "https://github.com/org/acme";
        for here in [
            "https://github.com/org/acme",
            "https://github.com/org/acme.git",
            "https://github.com/org/acme/",
            "git@github.com:org/acme.git",
            "git@github.com:org/acme",
            "ssh://git@github.com/org/acme.git",
            "ssh://git@ssh.github.com:443/org/acme",
            "ssh://git@ssh.github.com:443/org/acme.git",
            "SSH://git@SSH.GitHub.com:443/Org/Acme.git",
        ] {
            assert!(super::repository_urls_agree(live, here), "{here}");
            assert!(super::repository_urls_agree(here, live), "{here}");
        }
    }

    /// A different repository, owner, or forge host must still mismatch, so the
    /// comparison is `host + owner/repo`, never a prefix or host-only match.
    #[test]
    fn a_different_repository_or_host_still_mismatches() {
        let live = "https://github.com/org/acme";
        for here in [
            "ssh://git@ssh.github.com:443/org/other.git",
            "ssh://git@ssh.github.com:443/other/acme.git",
            "ssh://git@ssh.github.com:443/org/acme/extra.git",
            "git@gitlab.com:org/acme.git",
            "https://gitlab.com/org/acme",
            "ssh://git@ssh.gitlab.com:443/org/acme.git",
            "https://github.com/org",
            "",
        ] {
            assert!(!super::repository_urls_agree(live, here), "{here}");
        }
    }

    #[test]
    fn the_gate_accepts_the_fleet_ssh_origin() {
        disagreement(
            "acme",
            None,
            Some(&row("open", Some("https://github.com/org/acme"))),
            Some("ssh://git@ssh.github.com:443/org/acme"),
        )
        .unwrap();
    }

    #[test]
    fn ci_flag_needs_a_nonempty_ci_variable() {
        assert!(super::ci_flag_refusal(true, None).is_some());
        assert!(super::ci_flag_refusal(true, Some("")).is_some());
        assert!(super::ci_flag_refusal(true, Some("  ")).is_some());
        assert!(super::ci_flag_refusal(true, Some("true")).is_none());
        assert!(super::ci_flag_refusal(false, None).is_none());
        let message = super::ci_flag_refusal(true, None).unwrap();
        assert!(!message.contains('\n'), "one line: {message:?}");
    }

    #[test]
    fn only_a_numbered_pull_request_ref_can_open_the_pr_live_check() {
        assert!(is_pull_request_ref("refs/pull/7/merge", "/merge"));
        assert!(!is_pull_request_ref("refs/pull/0/merge", "/merge"));
        assert!(!is_pull_request_ref("refs/pull/7/head", "/merge"));
        assert!(!is_pull_request_ref("refs/heads/main", "/merge"));
        assert!(is_pull_request_ref("refs/pull/7/head", "/head"));
        assert!(!is_pull_request_ref("refs/pull/x/head", "/head"));
    }

    #[test]
    fn live_check_events_pair_with_their_intended_refs() {
        let github = CiForge::GitHub;
        assert!(can_mint_ci_session_for(
            github,
            "refs/pull/7/merge",
            "pull_request"
        ));
        assert!(!can_mint_ci_session_for(
            github,
            "refs/pull/7/merge",
            "pull_request_target"
        ));
        assert!(can_mint_ci_session_for(
            github,
            "refs/heads/main",
            "workflow_dispatch"
        ));
        assert!(!can_mint_ci_session_for(github, "refs/heads/topic", "push"));
    }

    /// Forgejo 15.0.9 runs a pull request on `refs/pull/<n>/head`; GitHub's
    /// merge-ref spelling is what Forgejo never signs, so it stays closed.
    #[test]
    fn a_forgejo_pull_request_opens_the_live_check_on_its_head_ref() {
        let forgejo = CiForge::Forgejo;
        assert!(can_mint_ci_session_for(
            forgejo,
            "refs/pull/7/head",
            "pull_request"
        ));
        assert!(!can_mint_ci_session_for(
            forgejo,
            "refs/pull/7/merge",
            "pull_request"
        ));
        assert!(!can_mint_ci_session_for(
            CiForge::GitHub,
            "refs/pull/7/head",
            "pull_request"
        ));
        assert!(can_mint_ci_session_for(forgejo, "refs/heads/main", "push"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_open_row_passes_through_oidc() {
        let _lock = ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let navigator = MockServer::start().await;
        let navigator_uri = navigator.uri();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();

        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        let previous_repo = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");
        std::env::set_var("GITHUB_REPOSITORY", "org/acme");

        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("audience", navigator_uri.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "github-jwt"
            })))
            .mount(&github)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .and(body_json(serde_json::json!({ "token": "github-jwt" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "navigator-ci-token",
                "exp": 1,
                "project_code": "acme"
            })))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header_matcher("authorization", "Bearer navigator-ci-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "code": "acme",
                    "status": "open",
                    "repository_url": "https://github.com/org/acme"
                }])),
            )
            .mount(&navigator)
            .await;

        let result = super::check_live(dir.path(), &navigator_uri).await;

        restore_env(previous_url, previous_token, previous_repo);
        result.expect("open live row must pass");
    }

    /// A Forgejo Actions run asks its own runner for the ID token (appending
    /// `audience` to a request URL that already carries a query) and trades it
    /// at the Forgejo door, never the GitHub one.
    #[tokio::test(flavor = "current_thread")]
    async fn a_forgejo_run_mints_at_the_forgejo_document_door() {
        let _lock = ENV_LOCK.lock().await;
        let forgejo = MockServer::start().await;
        let navigator = MockServer::start().await;
        let navigator_uri = navigator.uri();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();

        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        let previous_repo = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::set_var(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!(
                "{}/api/actions/_apis/pipelines/workflows/7/idtoken?placeholder=true",
                forgejo.uri()
            ),
        );
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "forgejo-runtime-token");
        std::env::remove_var("GITHUB_REPOSITORY");

        Mock::given(method("GET"))
            .and(path("/api/actions/_apis/pipelines/workflows/7/idtoken"))
            .and(query_param("placeholder", "true"))
            .and(query_param("audience", navigator_uri.as_str()))
            .and(header_matcher(
                "authorization",
                "Bearer forgejo-runtime-token",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "forgejo-jwt"
            })))
            .expect(1)
            .mount(&forgejo)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/forgejo/document-token"))
            .and(body_json(serde_json::json!({ "token": "forgejo-jwt" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "navigator-ci-token",
                "exp": 1,
                "project_code": "acme"
            })))
            .expect(1)
            .mount(&navigator)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header_matcher("authorization", "Bearer navigator-ci-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "code": "acme",
                    "status": "open",
                    "repository_url": "https://forge.example.test/firm/acme"
                }])),
            )
            .mount(&navigator)
            .await;

        let result = async {
            let (base, token) =
                crate::remote::resolve_ci_document_on(&navigator_uri, CiForge::Forgejo).await?;
            super::check_live_with_token(dir.path(), &base, &token).await
        }
        .await;

        restore_env(previous_url, previous_token, previous_repo);
        result.expect("a Forgejo run's open live row must pass");
    }

    /// Forgejo's runner exports the request variables empty when the job did
    /// not set `enable-openid-connect`; that is "unset", named as such.
    #[tokio::test(flavor = "current_thread")]
    async fn a_forgejo_job_without_openid_connect_is_told_which_grant_it_lacks() {
        let _lock = ENV_LOCK.lock().await;
        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        let previous_repo = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", "");
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "");

        let available = crate::remote::ci_oidc_available();
        let error =
            crate::remote::resolve_ci_document_on("http://127.0.0.1:9", CiForge::Forgejo).await;

        restore_env(previous_url, previous_token, previous_repo);
        assert!(!available);
        let message = format!("{:#}", error.expect_err("no token to request"));
        assert!(message.contains("Forgejo Actions"), "{message}");
        assert!(message.contains("enable-openid-connect: true"), "{message}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_missing_row_fails_through_oidc() {
        let _lock = ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let navigator = MockServer::start().await;
        let navigator_uri = navigator.uri();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();

        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        let previous_repo = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");
        std::env::remove_var("GITHUB_REPOSITORY");

        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("audience", navigator_uri.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "github-jwt"
            })))
            .mount(&github)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .and(body_json(serde_json::json!({ "token": "github-jwt" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "navigator-ci-token",
                "exp": 1,
                "project_code": "acme"
            })))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&navigator)
            .await;

        let error = super::check_live(dir.path(), &navigator_uri)
            .await
            .expect_err("empty list must fail");
        restore_env(previous_url, previous_token, previous_repo);
        assert!(error.to_string().contains("no live row"), "{error:#}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_closed_row_fails_through_oidc() {
        let _lock = ENV_LOCK.lock().await;
        let github = MockServer::start().await;
        let navigator = MockServer::start().await;
        let navigator_uri = navigator.uri();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();

        let previous_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok();
        let previous_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok();
        let previous_repo = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", github.uri());
        std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "github-oidc-request");
        std::env::set_var("GITHUB_REPOSITORY", "org/acme");

        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("audience", navigator_uri.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "github-jwt"
            })))
            .mount(&github)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/ci/document-token"))
            .and(body_json(serde_json::json!({ "token": "github-jwt" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "navigator-ci-token",
                "exp": 1,
                "project_code": "acme"
            })))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header_matcher("authorization", "Bearer navigator-ci-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "code": "acme",
                    "status": "closed",
                    "repository_url": "https://github.com/org/acme"
                }])),
            )
            .mount(&navigator)
            .await;

        let error = super::check_live(dir.path(), &navigator_uri)
            .await
            .expect_err("closed live row must fail without no_live_row");
        restore_env(previous_url, previous_token, previous_repo);
        let message = error.to_string();
        assert!(message.contains("live status is `closed`"), "{message}");
        assert!(message.contains("no_live_row"), "{message}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_refused_live_coordinate_read_fails_as_inconclusive() {
        let navigator = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header_matcher("authorization", "Bearer local-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "00000000-0000-0000-0000-000000000001", "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/app/api/project-surfaces/00000000-0000-0000-0000-000000000001",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_string("session scope refused"))
            .mount(&navigator)
            .await;

        let error = check_live_handles(dir.path(), &navigator.uri(), "local-token")
            .await
            .expect_err("a refused live read is never treated as a successful check");
        assert!(error.to_string().contains("403"), "{error:#}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_project_coordinates_match_the_manifest_read_only() {
        let navigator = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let project_id = "00000000-0000-0000-0000-000000000001";
        std::fs::write(
            dir.path().join("navigator.yaml"),
            concat!(
                "version: 26.9.26\nproject:\n",
                "  host: staging.neonlaw.com\n  name: acme\n",
                "  lawyer_dri: lawyer@example.com\n",
                "  client_dri: client@example.com\n",
                "  private_slack_channel: https://slack.example.com/private\n",
                "  private_notion_page: https://notion.example.com/private\n",
            ),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header_matcher("authorization", "Bearer local-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": project_id, "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/project-surfaces/{project_id}")))
            .and(header_matcher("authorization", "Bearer local-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "acme",
                "repository_url": "https://github.com/org/acme",
                "lawyer_dri": ["Lawyer@Example.com"],
                "client_dri": ["client@example.com"],
                "private_slack_channel": "https://slack.example.com/private",
                "private_notion_page": "https://notion.example.com/private"
            })))
            .mount(&navigator)
            .await;

        check_live_handles(dir.path(), &navigator.uri(), "local-token")
            .await
            .expect("the stored manifest coordinates match the live Project");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_live_coordinate_disagreement_refuses_the_gate() {
        let navigator = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let project_id = "00000000-0000-0000-0000-000000000001";
        std::fs::write(
            dir.path().join("navigator.yaml"),
            concat!(
                "version: 26.9.26\nproject:\n",
                "  host: staging.neonlaw.com\n  name: acme\n",
                "  lawyer_dri: lawyer@example.com\n",
                "  client_dri: client@example.com\n",
                "  private_slack_channel: https://slack.example.com/private\n",
                "  private_notion_page: https://notion.example.com/private\n",
            ),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": project_id, "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/project-surfaces/{project_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "acme",
                "repository_url": "https://github.com/org/acme",
                "lawyer_dri": ["lawyer@example.com"],
                "client_dri": ["someone-else@example.com"],
                "private_slack_channel": "https://slack.example.com/private",
                "private_notion_page": "https://notion.example.com/private"
            })))
            .mount(&navigator)
            .await;

        let error = check_live_handles(dir.path(), &navigator.uri(), "local-token")
            .await
            .expect_err("a coordinate mismatch fails closed");
        let message = error.to_string();
        assert!(message.contains("coordinates differ"), "{message}");
        assert!(
            message.contains("client_dri"),
            "the error must name which coordinate differs: {message}"
        );
        assert!(
            !message.contains("lawyer_dri") && !message.contains("private_slack_channel"),
            "the error must not name a coordinate that actually matched: {message}"
        );
    }

    /// A manifest `calendar_id` must name the live Project's calendar; a
    /// manifest without one makes no claim and passes.
    #[tokio::test(flavor = "current_thread")]
    async fn a_declared_calendar_must_match_the_live_project() {
        let project_id = "00000000-0000-0000-0000-000000000001";
        let base = concat!(
            "version: 26.9.26\nproject:\n",
            "  host: staging.neonlaw.com\n  name: acme\n",
            "  lawyer_dri: lawyer@example.com\n",
            "  client_dri: client@example.com\n",
            "  private_slack_channel: https://slack.example.com/private\n",
            "  private_notion_page: https://notion.example.com/private\n",
        );
        let navigator = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": project_id, "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/project-surfaces/{project_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "acme",
                "lawyer_dri": ["lawyer@example.com"],
                "client_dri": ["client@example.com"],
                "private_slack_channel": "https://slack.example.com/private",
                "private_notion_page": "https://notion.example.com/private",
                "calendar_id": "c_live@group.calendar.example.com"
            })))
            .mount(&navigator)
            .await;

        for (declared, agrees) in [
            (None, true),
            (Some("c_live@group.calendar.example.com"), true),
            (Some("c_other@group.calendar.example.com"), false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let manifest = match declared {
                Some(id) => format!("{base}  calendar_id: {id}\n"),
                None => base.to_string(),
            };
            std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
            let result = check_live_handles(dir.path(), &navigator.uri(), "local-token").await;
            match (agrees, result) {
                (true, Ok(())) => {}
                (false, Err(error)) => assert!(error.to_string().contains("calendar_id")),
                (agrees, result) => panic!("{declared:?}: expected {agrees}, got {result:?}"),
            }
        }
    }

    /// `Y016` lets `private_slack_channel`/`private_notion_page` carry the
    /// `not provisioned` sentinel for a Project with no live channel or
    /// page. The live gate must still check the claim: it agrees only when
    /// the live coordinate is genuinely absent.
    #[tokio::test(flavor = "current_thread")]
    async fn not_provisioned_agrees_with_a_genuinely_absent_live_coordinate() {
        let navigator = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let project_id = "00000000-0000-0000-0000-000000000001";
        std::fs::write(
            dir.path().join("navigator.yaml"),
            concat!(
                "version: 26.9.26\nproject:\n",
                "  host: staging.neonlaw.com\n  name: acme\n",
                "  lawyer_dri: lawyer@example.com\n",
                "  client_dri: client@example.com\n",
                "  private_slack_channel: not provisioned\n",
                "  private_notion_page: not provisioned\n",
            ),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": project_id, "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/project-surfaces/{project_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "acme",
                "repository_url": "https://github.com/org/acme",
                "lawyer_dri": ["lawyer@example.com"],
                "client_dri": ["client@example.com"],
                "private_slack_channel": null,
                "private_notion_page": null
            })))
            .mount(&navigator)
            .await;

        check_live_handles(dir.path(), &navigator.uri(), "local-token")
            .await
            .expect("`not provisioned` must agree with a genuinely absent live coordinate");
    }

    /// The sentinel must not be usable to paper over a coordinate that is
    /// actually live — the gate verifies the claim against reality rather
    /// than trusting the manifest.
    #[tokio::test(flavor = "current_thread")]
    async fn not_provisioned_does_not_paper_over_an_actual_live_coordinate() {
        let navigator = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let project_id = "00000000-0000-0000-0000-000000000001";
        std::fs::write(
            dir.path().join("navigator.yaml"),
            concat!(
                "version: 26.9.26\nproject:\n",
                "  host: staging.neonlaw.com\n  name: acme\n",
                "  lawyer_dri: lawyer@example.com\n",
                "  client_dri: client@example.com\n",
                "  private_slack_channel: not provisioned\n",
                "  private_notion_page: https://notion.example.com/private\n",
            ),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": project_id, "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/project-surfaces/{project_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "acme",
                "repository_url": "https://github.com/org/acme",
                "lawyer_dri": ["lawyer@example.com"],
                "client_dri": ["client@example.com"],
                "private_slack_channel": "https://slack.example.com/private",
                "private_notion_page": "https://notion.example.com/private"
            })))
            .mount(&navigator)
            .await;

        let error = check_live_handles(dir.path(), &navigator.uri(), "local-token")
            .await
            .expect_err("a live channel the manifest calls `not provisioned` must fail closed");
        let message = error.to_string();
        assert!(
            message.contains("private_slack_channel"),
            "the error must name the coordinate the sentinel could not paper over: {message}"
        );
        assert!(!message.contains("private_notion_page"), "{message}");
    }

    fn restore_env(
        previous_url: Option<String>,
        previous_token: Option<String>,
        previous_repo: Option<String>,
    ) {
        match previous_url {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_URL", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        }
        match previous_token {
            Some(value) => std::env::set_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN", value),
            None => std::env::remove_var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        }
        match previous_repo {
            Some(value) => std::env::set_var("GITHUB_REPOSITORY", value),
            None => std::env::remove_var("GITHUB_REPOSITORY"),
        }
    }

    /// The regression this module exists to catch: a document-scoped CI
    /// token's real `GET /app/api/projects` response, produced by the actual
    /// `portal::router` handler rather than hand-written JSON, must
    /// deserialize into [`LiveMatter`]. The other tests above mock the
    /// endpoint with a JSON literal that already includes every field this
    /// struct wants, which is exactly why the server's `DocumentProjectLookup`
    /// narrowing (introduced in 26.9.21) could drop `status` and
    /// `repository_url` for two releases without any test here noticing —
    /// see ENG-842. This test fails the same way `ci / verify` does the
    /// moment the two shapes diverge again.
    #[tokio::test]
    async fn document_scoped_projects_response_deserializes_into_live_matter() {
        let surreal = store::test_support::mem_surreal().await;
        let entity_id = store::test_support::seed_entity(&surreal).await;
        let project_id = store::projects::create(
            &surreal,
            &store::projects::NewProject {
                code: "acme".into(),
                name: "Acme".into(),
                status: "open".into(),
                entity_id,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        store::projects::set_repository_url(
            &surreal,
            project_id,
            Some("https://github.com/org/acme"),
        )
        .await
        .unwrap();
        let lawyer = store::persons::create(
            &surreal,
            &store::persons::NewPerson::with_role(
                "Synthetic Lawyer",
                "ci-lawyer@example.com",
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        store::projects::designate_dri_in_surreal(
            &surreal,
            project_id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();

        let mut state = portal::test_support::app_state(surreal).await;
        state.canonical_host = portal::CanonicalHost::new(Some("staging.neonlaw.com".into()));
        state.github_oidc = GitHubOidc::fixed(GitHubActionsClaims {
            sub: "repo:org/acme:ref:refs/heads/main".into(),
            repository: "org/acme".into(),
            repository_owner: "org".into(),
            git_ref: "refs/heads/main".into(),
            event_name: "push".into(),
            jti: "jti-document-acme".into(),
            exp: 4_000_000_000,
            ..Default::default()
        });
        let app = portal::router(state);

        let mint_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/ci/document-token")
                    .header(header::HOST, "staging.neonlaw.com")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"token":"github-jwt"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mint_response.status(), axum::http::StatusCode::OK);
        let mint_body = axum::body::to_bytes(mint_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let minted: serde_json::Value = serde_json::from_slice(&mint_body).unwrap();
        let token = minted["token"].as_str().unwrap();

        let projects_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/app/api/projects")
                    .header(header::HOST, "staging.neonlaw.com")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(projects_response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(projects_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        let matters: Vec<LiveMatter> =
            serde_json::from_str(&body).expect("the live door's own response must parse");
        let acme = matters
            .iter()
            .find(|matter| matter.code == "acme")
            .expect("the seeded project is in its own lookup");
        assert_eq!(acme.status, "open");
        assert_eq!(
            acme.repository_url.as_deref(),
            Some("https://github.com/org/acme")
        );
    }

    /// A `pages:` block declaring a deadline board, a key-dates strip, and a
    /// file list, for the `Y023` tests.
    const Y023_MANIFEST: &str = concat!(
        "version: 26.10.6\nproject:\n  host: h.example.com\n  name: acme\n",
        "pages:\n",
        "  - slug: timeline\n    kind: docket_deadline_board\n    title: Case timeline\n",
        "    lenses:\n",
        "      lawyer: [deadline_board, boundary_note, provenance_statement]\n",
        "      client: [key_dates_strip, boundary_note, provenance_statement]\n",
    );
    const Y023_PROJECT: &str = "00000000-0000-0000-0000-0000000000a1";

    async fn y023_host(deadlines: serde_json::Value, status: u16) -> MockServer {
        let navigator = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": Y023_PROJECT, "code": "acme" }
            ])))
            .mount(&navigator)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{Y023_PROJECT}/section-counts"
            )))
            .respond_with(ResponseTemplate::new(status).set_body_json(deadlines))
            .mount(&navigator)
            .await;
        navigator
    }

    fn y023_dir(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
        dir
    }

    async fn assert_only_reads(navigator: &MockServer) {
        let requests = navigator.received_requests().await.unwrap();
        assert!(!requests.is_empty());
        for request in requests {
            assert_eq!(request.method.as_str(), "GET", "{}", request.url);
        }
    }

    /// ENG-1060: a populated matter is quiet; the deadline source is asked
    /// once for both sections that read it.
    #[tokio::test(flavor = "current_thread")]
    async fn y023_a_populated_matter_has_no_finding() {
        let navigator = y023_host(
            serde_json::json!({ "docket": 0, "deadlines": 1, "documents": 0 }),
            200,
        )
        .await;
        let dir = y023_dir(Y023_MANIFEST);
        let findings =
            super::section_data_findings(dir.path(), &navigator.uri(), "local-token").await;
        assert!(findings.is_empty(), "{findings:?}");
        let deadline_reads = navigator
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path().ends_with("/section-counts"))
            .count();
        assert_eq!(deadline_reads, 1);
        assert_only_reads(&navigator).await;
    }

    /// `pages init` asks the same sources: open Deadlines name both sections
    /// that read them, and a source the host refuses names nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn populated_sections_names_the_sections_of_sources_with_rows() {
        let navigator = y023_host(
            serde_json::json!({ "docket": 0, "deadlines": 1, "documents": 0 }),
            200,
        )
        .await;
        let id = uuid::Uuid::parse_str(Y023_PROJECT).unwrap();
        let sections = super::populated_sections(&navigator.uri(), "local-token", id).await;
        assert_eq!(sections, ["deadline_board", "key_dates_strip"]);
        assert_only_reads(&navigator).await;
    }

    /// ENG-1060: an empty matter warns once per declared section, at the
    /// line that names it, with the command that adds a row. Only open
    /// deadlines count, because only they render on the board.
    #[tokio::test(flavor = "current_thread")]
    async fn y023_an_empty_matter_warns_with_the_fix_command() {
        let navigator = y023_host(
            serde_json::json!({ "docket": 0, "deadlines": 0, "documents": 0 }),
            200,
        )
        .await;
        let dir = y023_dir(Y023_MANIFEST);
        let findings =
            super::section_data_findings(dir.path(), &navigator.uri(), "local-token").await;
        assert_eq!(findings.len(), 2, "{findings:?}");
        let board = findings
            .iter()
            .find(|finding| finding.message.contains("`deadline_board`"))
            .unwrap();
        assert_eq!(board.line, 10, "the lawyer lens line names the section");
        assert!(
            board.message.contains("navigator project deadline create"),
            "{}",
            board.message
        );
        let strip = findings
            .iter()
            .find(|finding| finding.message.contains("`key_dates_strip`"))
            .unwrap();
        assert_eq!(strip.line, 11);
        let rendered = board.render();
        assert!(rendered.starts_with("warning:"), "{rendered}");
        assert!(rendered.contains("Y023"), "{rendered}");
        assert_only_reads(&navigator).await;
    }

    /// ENG-1060 policy: `Y023` is a warning on every event, and a refused
    /// read (a CI session whose scope does not reach the door) is an
    /// inconclusive warning, never an error and never a silent pass.
    #[tokio::test(flavor = "current_thread")]
    async fn y023_is_a_warning_and_a_refused_read_is_inconclusive() {
        assert_eq!(
            super::SectionDataFinding::SEVERITY,
            rules::Severity::Warning
        );
        let navigator = y023_host(serde_json::json!({ "error": "forbidden" }), 403).await;
        let dir = y023_dir(Y023_MANIFEST);
        let findings =
            super::section_data_findings(dir.path(), &navigator.uri(), "local-token").await;
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].message.contains("inconclusive"),
            "{}",
            findings[0].message
        );
        assert!(findings[0].render().starts_with("warning:"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_section_counts_are_inconclusive_never_empty() {
        for body in [
            serde_json::json!({"docket":0,"deadlines":0}),
            serde_json::json!({"docket":0,"deadlines":-1,"documents":0}),
            serde_json::json!({"docket":0,"deadlines":"0","documents":0}),
            serde_json::json!({"docket":0,"deadlines":0.5,"documents":0}),
        ] {
            let navigator = y023_host(body, 200).await;
            let dir = y023_dir(Y023_MANIFEST);
            let findings =
                super::section_data_findings(dir.path(), &navigator.uri(), "token").await;
            assert_eq!(findings.len(), 1);
            assert!(findings[0].message.contains("inconclusive"));
            assert!(!findings[0].message.contains("has no"));
        }
    }

    /// A manifest without `pages:`, or one that declares `no_live_row`, asks
    /// the deployment nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn y023_asks_nothing_without_pages_or_a_live_row() {
        let navigator = MockServer::start().await;
        let plain = y023_dir("version: 26.10.6\nproject:\n  host: h.example.com\n  name: acme\n");
        assert!(
            super::section_data_findings(plain.path(), &navigator.uri(), "t")
                .await
                .is_empty()
        );
        let rowless = y023_dir(&format!("{Y023_MANIFEST}no_live_row: the matter closed\n"));
        assert!(
            super::section_data_findings(rowless.path(), &navigator.uri(), "t")
                .await
                .is_empty()
        );
        assert!(navigator.received_requests().await.unwrap().is_empty());
    }
}
