//! Mint an explicitly scoped CI session from a GitHub Actions OIDC token.
//!
//! `POST /auth/ci/seed-token` and `POST /auth/ci/document-token` are the CI
//! counterparts of `/auth/cli/start`. A Project repository's job presents
//! GitHub's JWT; the door verifies it, binds the run to the live Project
//! whose `repository_url` is that repository (or, in the deployment's own
//! GitHub organization, to the Project of that name whose `repository_url` is
//! on the trusted Forgejo; see [`resolve_project`]), and returns an HMAC-signed
//! [`SessionData`] attributed to that Project's own lawyer DRI. Each door
//! carries an explicit capability: the seed mint can reach its one write
//! endpoint, while the document mint can reach only the metadata reads
//! `navigator project gate --check` uses.
//!
//! Forgejo runs have their own pair of doors. `/auth/ci/forgejo/seed-token`
//! writes only for the deployment-trusted CD workflow on `refs/heads/main`;
//! the gate on a pull request gets a dry-run seed session, and every other
//! Forgejo run is refused.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use store::seed::SeedModel;
use views::brand::{BrandKey, admitted_brand_key, held_out_host};

use crate::CanonicalHost;
use crate::forgejo_oidc::{ForgejoOidc, ForgejoRun, repository_matches_issuer};
use crate::github_oidc::{GitHubActionsClaims, GitHubOidc};
use crate::session::{
    CI_SESSION_TTL_SECS, DocumentScope, SeedScope, SessionData, SessionScope, SessionSource,
    SessionStore, now_unix_secs, random_token_32,
};

/// Shared state for the CI mint door.
#[derive(Clone)]
pub struct CiAuthState {
    pub sessions: SessionStore,
    pub surreal: store::surreal::SurrealDb,
    pub github_oidc: GitHubOidc,
    pub forgejo_oidc: ForgejoOidc,
    pub canonical_host: CanonicalHost,
    /// The deployment's `NAVIGATOR_GITHUB_ORG`. Unset or blank disables the
    /// GitHub door's Forgejo-URL fallback.
    pub github_org: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MintRequest {
    token: String,
}

#[derive(Debug, Serialize)]
struct MintResponse {
    token: String,
    exp: i64,
    project_code: String,
}

/// Build the `/auth/ci/*` sub-router.
pub fn routes(state: CiAuthState) -> Router {
    Router::new()
        .route("/auth/ci/seed-token", post(mint_seed_token))
        .route("/auth/ci/document-token", post(mint_document_token))
        .route(
            "/auth/ci/forgejo/document-token",
            post(mint_forgejo_document_token),
        )
        .route("/auth/ci/forgejo/seed-token", post(mint_forgejo_seed_token))
        .with_state(state)
}

async fn mint_seed_token(
    State(state): State<CiAuthState>,
    headers: HeaderMap,
    Json(input): Json<MintRequest>,
) -> Response {
    match mint_inner(
        &state,
        &headers,
        &input.token,
        "ci.seed_token.minted",
        CiSessionKind::Seed,
    )
    .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /auth/ci/document-token` — the `navigator project gate --check --ci`
/// exchange. The minted session is limited to the named Project's lookup,
/// revision metadata, and document-integrity reads.
async fn mint_document_token(
    State(state): State<CiAuthState>,
    headers: HeaderMap,
    Json(input): Json<MintRequest>,
) -> Response {
    match mint_inner(
        &state,
        &headers,
        &input.token,
        "ci.document_token.minted",
        CiSessionKind::Document,
    )
    .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /auth/ci/forgejo/document-token` — Forgejo's separate, deployment-
/// configured document metadata exchange. It cannot mint the seed capability.
async fn mint_forgejo_document_token(
    State(state): State<CiAuthState>,
    Json(input): Json<MintRequest>,
) -> Response {
    match mint_forgejo_inner(&state, &input.token, CiSessionKind::Document).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /auth/ci/forgejo/seed-token` — the Forgejo half of the seed door.
/// Only the deployment-trusted CD workflow on `refs/heads/main` mints a
/// writing session; the gate on a pull request mints a dry-run-only one.
async fn mint_forgejo_seed_token(
    State(state): State<CiAuthState>,
    Json(input): Json<MintRequest>,
) -> Response {
    match mint_forgejo_inner(&state, &input.token, CiSessionKind::Seed).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

enum MintError {
    Unavailable(&'static str),
    Unauthorized(String),
    Forbidden(String),
    Internal(String),
}

impl IntoResponse for MintError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            Self::Unavailable(message) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                message.to_string(),
            ),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, "unauthorized", message),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, "forbidden", message),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", message),
        };
        (
            status,
            Json(serde_json::json!({ "error": error, "message": message })),
        )
            .into_response()
    }
}

fn github_mint_origin(
    canonical: &CanonicalHost,
    headers: &HeaderMap,
) -> Result<(String, Option<BrandKey>), MintError> {
    let Some(deployment) = canonical.host() else {
        return Err(MintError::Unavailable(
            "CANONICAL_HOST is required to mint CI tokens",
        ));
    };
    let refused = || MintError::Unauthorized("CI host is not admitted by this deployment".into());
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .ok_or_else(refused)?;
    let brand = admitted_brand_key(host);
    if host.eq_ignore_ascii_case(deployment) && !held_out_host(host) {
        return Ok((format!("https://{deployment}"), brand));
    }
    let brand = brand
        .filter(|_| admitted_brand_key(deployment).is_some())
        .ok_or_else(refused)?;
    let audience = brand.public_home_href_for(Some(&deployment.to_ascii_lowercase()));
    if !audience.eq_ignore_ascii_case(&format!("https://{host}")) {
        return Err(refused());
    }
    Ok((audience, Some(brand)))
}

async fn mint_inner(
    state: &CiAuthState,
    headers: &HeaderMap,
    github_token: &str,
    audit_event: &'static str,
    kind: CiSessionKind,
) -> Result<MintResponse, MintError> {
    let (audience, brand) = github_mint_origin(&state.canonical_host, headers)?;
    let claims = state
        .github_oidc
        .verify(github_token, &audience)
        .await
        .map_err(|error| MintError::Unauthorized(error.to_string()))?;
    authorize_github_run(&claims)?;
    state
        .github_oidc
        .spend_jti(&claims.jti, claims.exp)
        .map_err(|error| MintError::Unauthorized(error.to_string()))?;
    let project = resolve_project(state, &claims).await?;
    if brand.is_some_and(|brand| brand.as_str() != project.brand) {
        return Err(MintError::Forbidden(UNBOUND.into()));
    }
    let actor = lawyer_dri_actor(&state.surreal, &project, kind).await?;
    let exp = now_unix_secs() + CI_SESSION_TTL_SECS;
    let session = SessionData {
        sub: actor
            .oidc_subject
            .clone()
            .unwrap_or_else(|| actor.id.to_string()),
        email: Some(actor.email.clone()),
        person_id: Some(actor.id),
        exp,
        role: actor.role,
        csrf_token: random_token_32(),
        source: SessionSource::Ci,
        provider: None,
        viewing_as_dri: None,
        email_verified: false,
        scope: Some(match kind {
            CiSessionKind::Seed => SessionScope::Seed(SeedScope {
                endpoint: crate::api::SEED_ENDPOINT.to_string(),
                models: SeedModel::ALL.to_vec(),
                project_code: project.code.clone(),
                dry_run_only: is_pull_request_run(&claims),
            }),
            CiSessionKind::Document => SessionScope::Document(DocumentScope {
                project_id: project.id,
                project_code: project.code.clone(),
            }),
        }),
    };
    let token = state.sessions.encode(&session);
    tracing::info!(
        target: "audit",
        event = audit_event,
        project_code = %project.code,
        person_id = %actor.id,
        repository = %claims.repository,
        "ci: minted a project-scoped session",
    );
    Ok(MintResponse {
        token,
        exp,
        project_code: project.code,
    })
}

async fn mint_forgejo_inner(
    state: &CiAuthState,
    token: &str,
    kind: CiSessionKind,
) -> Result<MintResponse, MintError> {
    let claims = state
        .forgejo_oidc
        .verify(token)
        .await
        .map_err(|error| MintError::Unauthorized(error.to_string()))?;
    let run = state
        .forgejo_oidc
        .authorize_run(&claims)
        .map_err(|error| MintError::Forbidden(error.to_string()))?;
    // Decided before the token is spent, so a refused run burns nothing.
    let seed_dry_run_only = match (kind, run) {
        (CiSessionKind::Seed, ForgejoRun::MainCd) => false,
        (CiSessionKind::Seed, ForgejoRun::MainGate) => {
            return Err(MintError::Forbidden(
                "only the trusted Forgejo CD workflow on main may write seeds".into(),
            ));
        }
        // A pull request's seed session is dry-run; a document session never writes.
        (CiSessionKind::Seed, ForgejoRun::PullRequest) | (CiSessionKind::Document, _) => true,
    };
    state
        .forgejo_oidc
        .spend_token(token, claims.exp)
        .map_err(|error| MintError::Unauthorized(error.to_string()))?;
    let project = resolve_forgejo_project(&state.surreal, &claims.iss, &claims.repository).await?;
    let actor = lawyer_dri_actor(&state.surreal, &project, kind).await?;
    let exp = (now_unix_secs() + CI_SESSION_TTL_SECS).min(claims.exp);
    let session = SessionData {
        sub: actor
            .oidc_subject
            .clone()
            .unwrap_or_else(|| actor.id.to_string()),
        email: Some(actor.email.clone()),
        person_id: Some(actor.id),
        exp,
        role: actor.role,
        csrf_token: random_token_32(),
        source: SessionSource::Ci,
        provider: None,
        viewing_as_dri: None,
        email_verified: false,
        scope: Some(match kind {
            CiSessionKind::Seed => SessionScope::Seed(SeedScope {
                endpoint: crate::api::SEED_ENDPOINT.to_string(),
                models: SeedModel::ALL.to_vec(),
                project_code: project.code.clone(),
                dry_run_only: seed_dry_run_only,
            }),
            CiSessionKind::Document => SessionScope::Document(DocumentScope {
                project_id: project.id,
                project_code: project.code.clone(),
            }),
        }),
    };
    tracing::info!(
        target: "audit",
        event = match kind {
            CiSessionKind::Seed => "ci.seed_token.minted",
            CiSessionKind::Document => "ci.document_token.minted",
        },
        project_code = %project.code,
        person_id = %actor.id,
        repository = %claims.repository,
        forgejo_run_id = %claims.run_id,
        dry_run_only = seed_dry_run_only,
        "forgejo ci: minted a project-scoped session",
    );
    Ok(MintResponse {
        token: state.sessions.encode(&session),
        exp,
        project_code: project.code,
    })
}

fn authorize_github_run(claims: &GitHubActionsClaims) -> Result<(), MintError> {
    if claims.git_ref == "refs/heads/main"
        && matches!(claims.event_name.as_str(), "push" | "workflow_dispatch")
    {
        return Ok(());
    }
    if is_pull_request_run(claims) {
        return Ok(());
    }
    Err(MintError::Forbidden(
        "GitHub Actions OIDC token must come from a push or workflow_dispatch on main, or a pull_request merge ref".into(),
    ))
}

fn is_pull_request_run(claims: &GitHubActionsClaims) -> bool {
    let Some(number) = claims
        .git_ref
        .strip_prefix("refs/pull/")
        .and_then(|reference| reference.strip_suffix("/merge"))
    else {
        return false;
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(number) = number.parse::<u64>() else {
        return false;
    };
    number > 0
        && claims.event_name == "pull_request"
        && subject_repository(&claims.sub).as_deref() == Some(claims.repository.as_str())
}

/// Extract `owner/name` from a `pull_request` subject claim, accepting both
/// GitHub's classic spelling (`repo:owner/name:pull_request`) and the
/// immutable-subject spelling (`repo:owner@id/name@id:pull_request`) that
/// repositories with immutable subject claims enabled send instead. The
/// subject is parsed field-by-field rather than compared as an opaque
/// string, since the two spellings are never byte-equal for the same repo.
fn subject_repository(sub: &str) -> Option<String> {
    let repo = sub.strip_prefix("repo:")?.strip_suffix(":pull_request")?;
    let (owner, name) = repo.split_once('/')?;
    let owner = owner.split('@').next().filter(|part| !part.is_empty())?;
    let name = name.split('@').next().filter(|part| !part.is_empty())?;
    Some(format!("{owner}/{name}"))
}

/// Shared refusal when this GitHub run cannot be bound to exactly one live
/// Project. The message names nothing about whether a repository or row
/// exists.
const UNBOUND: &str = "this GitHub Actions run is not bound to a live project";

/// Bind a GitHub run to exactly one live Project: the one whose
/// `repository_url` is this GitHub repository. Only when none is, a run whose
/// owner is the deployment's GitHub organization binds to the one Project
/// named for the repository whose `repository_url` is a repository of that
/// name on the trusted Forgejo installation. That owner pin keeps a
/// same-named repository under any other GitHub owner out.
async fn resolve_project(
    state: &CiAuthState,
    claims: &GitHubActionsClaims,
) -> Result<store::projects::Project, MintError> {
    let Some((owner, code)) = claims.repository.split_once('/') else {
        return Err(MintError::Forbidden(UNBOUND.into()));
    };
    if owner != claims.repository_owner || code.is_empty() {
        return Err(MintError::Forbidden(UNBOUND.into()));
    }
    let projects = store::projects::all(&state.surreal)
        .await
        .map_err(|error| MintError::Internal(error.to_string()))?;
    let mut matches: Vec<&store::projects::Project> = projects
        .iter()
        .filter(|project| {
            project
                .repository_url
                .as_deref()
                .and_then(github_repository_from_url)
                .as_deref()
                == Some(claims.repository.as_str())
        })
        .collect();
    let in_deployment_org = state
        .github_org
        .as_deref()
        .map(str::trim)
        .is_some_and(|org| !org.is_empty() && org == owner);
    if matches.is_empty() && in_deployment_org {
        matches = projects
            .iter()
            .filter(|project| {
                project.code == code
                    && project
                        .repository_url
                        .as_deref()
                        .is_some_and(|url| state.forgejo_oidc.hosts_repository_named(url, code))
            })
            .collect();
    }
    let [project] = matches.as_slice() else {
        return Err(MintError::Forbidden(UNBOUND.into()));
    };
    if project.code != code {
        return Err(MintError::Forbidden(UNBOUND.into()));
    }
    Ok((*project).clone())
}

async fn resolve_forgejo_project(
    surreal: &store::surreal::SurrealDb,
    issuer: &str,
    repository: &str,
) -> Result<store::projects::Project, MintError> {
    let Some((_, name)) = repository.split_once('/') else {
        return Err(MintError::Forbidden(
            "this Forgejo Actions run is not bound to a live project".into(),
        ));
    };
    let matches: Vec<store::projects::Project> = store::projects::all(surreal)
        .await
        .map_err(|error| MintError::Internal(error.to_string()))?
        .into_iter()
        .filter(|project| {
            project.code == name
                && project
                    .repository_url
                    .as_deref()
                    .is_some_and(|url| repository_matches_issuer(url, issuer, repository))
        })
        .collect();
    let [project] = matches.as_slice() else {
        return Err(MintError::Forbidden(
            "this Forgejo Actions run is not bound to a live project".into(),
        ));
    };
    Ok(project.clone())
}

#[derive(Clone, Copy)]
enum CiSessionKind {
    Seed,
    Document,
}

async fn lawyer_dri_actor(
    surreal: &store::surreal::SurrealDb,
    project: &store::projects::Project,
    kind: CiSessionKind,
) -> Result<store::persons::Person, MintError> {
    let people = store::projects::lawyer_dri_people(surreal, project.id)
        .await
        .map_err(|error| MintError::Internal(error.to_string()))?;
    let Some(person) = people
        .into_iter()
        .find(|person| person.role.is_lawyer_tier())
    else {
        let purpose = if matches!(kind, CiSessionKind::Seed) {
            "attribute a CI seed write to"
        } else {
            "attribute CI document verification to"
        };
        return Err(MintError::Forbidden(format!(
            "the live project has no lawyer-tier lawyer DRI to {purpose}"
        )));
    };
    Ok(person)
}

/// Extract `owner/name` from a GitHub HTTPS or SSH remote.
#[must_use]
pub fn github_repository_from_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_start_matches('/');
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|part| !part.is_empty())?;
    let name = parts.next().filter(|part| !part.is_empty())?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::{CiAuthState, authorize_github_run, github_repository_from_url, routes};
    use crate::CanonicalHost;
    use crate::forgejo_oidc::ForgejoOidc;
    use crate::github_oidc::{GitHubActionsClaims, GitHubOidc};
    use crate::session::{SessionStore, now_unix_secs};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn github_mint_selects_only_a_live_host_in_the_configured_deployment() {
        use views::brand::BrandKey;

        let hosts = |deployment: &str, host: &str| {
            let canonical = CanonicalHost::new(Some(deployment.into()));
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(axum::http::header::HOST, host.parse().unwrap());
            super::github_mint_origin(&canonical, &headers).ok()
        };
        assert_eq!(
            hosts("STAGING.neonlaw.com", "staging-iep.neonlaw.com"),
            Some((
                "https://staging-iep.neonlaw.com".into(),
                Some(BrandKey::IepDefenders)
            ))
        );
        assert_eq!(
            hosts("www.neonlaw.com", "iep.neonlaw.com"),
            Some((
                "https://iep.neonlaw.com".into(),
                Some(BrandKey::IepDefenders)
            ))
        );
        for host in [
            "iep.neonlaw.com",
            "unknown.example",
            "staging-iep.neonlaw.com:8443",
        ] {
            assert_eq!(hosts("staging.neonlaw.com", host), None, "{host}");
        }
    }

    #[test]
    fn github_https_and_ssh_remotes_yield_owner_name() {
        assert_eq!(
            github_repository_from_url("https://github.com/neon-law-staging/acme.git"),
            Some("neon-law-staging/acme".into())
        );
        assert_eq!(
            github_repository_from_url("git@github.com:neon-law-staging/acme"),
            Some("neon-law-staging/acme".into())
        );
        assert_eq!(
            github_repository_from_url("https://gitlab.example/acme"),
            None
        );
    }

    #[test]
    fn a_pull_request_run_is_authorized_only_for_the_merge_ref_shape() {
        let claims = GitHubActionsClaims {
            sub: "repo:neon-law-staging/acme:pull_request".into(),
            repository: "neon-law-staging/acme".into(),
            repository_owner: "neon-law-staging".into(),
            git_ref: "refs/pull/42/merge".into(),
            event_name: "pull_request".into(),
            ..GitHubActionsClaims::default()
        };
        assert!(authorize_github_run(&claims).is_ok());
    }

    #[test]
    fn a_pull_request_run_with_an_immutable_subject_is_authorized() {
        let claims = GitHubActionsClaims {
            sub: "repo:neon-law-staging@318426496/sample-litigation@1336521864:pull_request".into(),
            repository: "neon-law-staging/sample-litigation".into(),
            repository_owner: "neon-law-staging".into(),
            git_ref: "refs/pull/42/merge".into(),
            event_name: "pull_request".into(),
            ..GitHubActionsClaims::default()
        };
        assert!(authorize_github_run(&claims).is_ok());
    }

    #[test]
    fn a_pull_request_run_rejects_branch_tag_and_untrusted_event_shapes() {
        for (git_ref, event_name, sub) in [
            (
                "refs/heads/main",
                "pull_request",
                "repo:neon-law-staging/acme:pull_request",
            ),
            (
                "refs/tags/v1",
                "pull_request",
                "repo:neon-law-staging/acme:pull_request",
            ),
            (
                "refs/pull/42/merge",
                "pull_request",
                "repo:neon-law-staging/acme:ref:refs/heads/main",
            ),
            (
                "refs/pull/42/merge",
                "pull_request_target",
                "repo:neon-law-staging/acme:pull_request",
            ),
            (
                "refs/pull/42/merge",
                "pull_request",
                "repo:someone-else/other-repo:pull_request",
            ),
            (
                "refs/pull/42/merge",
                "pull_request",
                "repo:someone-else@1/other-repo@2:pull_request",
            ),
        ] {
            let claims = GitHubActionsClaims {
                sub: sub.into(),
                repository: "neon-law-staging/acme".into(),
                repository_owner: "neon-law-staging".into(),
                git_ref: git_ref.into(),
                event_name: event_name.into(),
                ..GitHubActionsClaims::default()
            };
            assert!(
                authorize_github_run(&claims).is_err(),
                "unexpectedly authorized {event_name} {git_ref} {sub}"
            );
        }
    }

    #[test]
    fn a_non_main_ref_is_not_a_seed_write() {
        let claims = GitHubActionsClaims {
            git_ref: "refs/heads/topic".into(),
            ..GitHubActionsClaims::default()
        };
        assert!(authorize_github_run(&claims).is_err());
    }

    /// A live Project bound to `claims.repository`, carrying no lawyer-tier
    /// lawyer DRI — the shared refusal both doors below hit.
    async fn project_with_no_lawyer_dri(
        surreal: &store::surreal::SurrealDb,
        code: &str,
        brand: &str,
    ) -> store::projects::Project {
        if store::brands::find_by_key(surreal, brand)
            .await
            .unwrap()
            .is_none()
        {
            store::test_support::seed_compiled_brands_postgres(surreal).await;
        }
        let entity_id = store::test_support::seed_entity_with_parents(surreal).await;
        let project = store::projects::create(
            surreal,
            &store::projects::NewProject {
                code: code.to_string(),
                brand: brand.into(),
                name: code.to_string(),
                status: "open".to_string(),
                entity_id,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        store::projects::set_repository_url(
            surreal,
            project.id,
            Some(&format!("https://github.com/neon-law-staging/{code}")),
        )
        .await
        .unwrap();
        project
    }

    fn ci_state(surreal: store::surreal::SurrealDb, repository: &str, jti: &str) -> CiAuthState {
        CiAuthState {
            sessions: SessionStore::new("test-ci-auth-session-key"),
            surreal,
            github_oidc: GitHubOidc::fixed(GitHubActionsClaims {
                repository: repository.to_string(),
                repository_owner: "neon-law-staging".to_string(),
                jti: jti.to_string(),
                exp: now_unix_secs() + 60,
                ..GitHubActionsClaims::default()
            }),
            forgejo_oidc: ForgejoOidc::rejecting(),
            canonical_host: CanonicalHost::new(Some("staging.neonlaw.com".to_string())),
            github_org: None,
        }
    }

    /// A live Project `acme` with a lawyer DRI, recorded at `repository_url`.
    async fn project_at(surreal: &store::surreal::SurrealDb, repository_url: &str, brand: &str) {
        if let Some(project) = store::projects::all(surreal)
            .await
            .unwrap()
            .into_iter()
            .find(|project| project.code == "acme")
        {
            store::projects::set_repository_url(surreal, project.id, Some(repository_url))
                .await
                .unwrap();
            return;
        }
        let project = project_with_no_lawyer_dri(surreal, "acme", brand).await;
        store::projects::set_repository_url(surreal, project.id, Some(repository_url))
            .await
            .unwrap();
        let lawyer = store::persons::create(
            surreal,
            &store::persons::NewPerson::with_role(
                "Lawyer Example",
                "lawyer@example.com",
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        store::projects::designate_dri_in_surreal(
            surreal,
            project.id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
    }

    /// A GitHub run of `<owner>/acme` against a deployment whose GitHub
    /// organization is `org` and whose Forgejo issuer is `forgejo.example`.
    async fn forgejo_fallback_mint(
        surreal: &store::surreal::SurrealDb,
        repository_url: &str,
        owner: &str,
        org: Option<&str>,
        forgejo_oidc: ForgejoOidc,
    ) -> (StatusCode, serde_json::Value) {
        project_at(surreal, repository_url, "neon").await;
        let mut state = ci_state(surreal.clone(), &format!("{owner}/acme"), "fallback-jti");
        state.github_oidc = GitHubOidc::fixed(GitHubActionsClaims {
            repository: format!("{owner}/acme"),
            repository_owner: owner.to_string(),
            git_ref: "refs/heads/main".into(),
            event_name: "push".into(),
            jti: "fallback-jti".into(),
            exp: now_unix_secs() + 60,
            ..GitHubActionsClaims::default()
        });
        state.github_org = org.map(str::to_string);
        state.forgejo_oidc = forgejo_oidc;
        mint_request(state, "/auth/ci/document-token").await
    }

    fn trusted_forgejo() -> ForgejoOidc {
        ForgejoOidc::configured(
            "https://forgejo.example/api/actions",
            "navigator-ci",
            ".forgejo/workflows/project-gate.yml",
        )
        .unwrap()
    }

    const FORGEJO_URL: &str = "https://forgejo.example/example-forge-org/acme";

    #[tokio::test]
    async fn the_deployment_org_binds_a_project_recorded_at_its_trusted_forgejo_url() {
        store::test_support::with_required_postgres(async {
            let db = store::surreal::SurrealDb::uninitialized();
            let (status, body) = forgejo_fallback_mint(
                &db,
                FORGEJO_URL,
                "example-org",
                Some("example-org"),
                trusted_forgejo(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["project_code"], "acme");
        })
        .await;
    }

    #[tokio::test]
    async fn the_exact_github_url_binds_without_the_fallback() {
        store::test_support::with_required_postgres(async {
            let db = store::surreal::SurrealDb::uninitialized();
            let (status, body) = forgejo_fallback_mint(
                &db,
                "https://github.com/example-org/acme",
                "example-org",
                None,
                ForgejoOidc::rejecting(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["project_code"], "acme");
        })
        .await;
    }

    #[tokio::test]
    async fn the_forgejo_fallback_refuses_every_run_it_does_not_trust() {
        store::test_support::with_required_postgres(async {
            let db = store::surreal::SurrealDb::uninitialized();
            for (case, repository_url, owner, org, forgejo_oidc) in [
                (
                    "a same-named repository under a foreign owner",
                    FORGEJO_URL,
                    "someone-else",
                    Some("example-org"),
                    trusted_forgejo(),
                ),
                (
                    "a Project URL on an untrusted host",
                    "https://other.example/example-forge-org/acme",
                    "example-org",
                    Some("example-org"),
                    trusted_forgejo(),
                ),
                (
                    "an unset GitHub organization",
                    FORGEJO_URL,
                    "example-org",
                    None,
                    trusted_forgejo(),
                ),
                (
                    "a blank GitHub organization",
                    FORGEJO_URL,
                    "example-org",
                    Some("  "),
                    trusted_forgejo(),
                ),
                (
                    "no trusted Forgejo issuer",
                    FORGEJO_URL,
                    "example-org",
                    Some("example-org"),
                    ForgejoOidc::rejecting(),
                ),
            ] {
                let (status, body) =
                    forgejo_fallback_mint(&db, repository_url, owner, org, forgejo_oidc).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{case}: {body}");
                assert_eq!(body["message"], super::UNBOUND, "{case}");
            }
        })
        .await;
    }

    async fn mint_request(state: CiAuthState, path: &str) -> (StatusCode, serde_json::Value) {
        mint_at(state, path, "staging.neonlaw.com", "anything").await
    }

    async fn mint_at(
        state: CiAuthState,
        path: &str,
        host: &str,
        token: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = routes(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", host)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"token":token}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn github_mint_binds_the_signed_audience_and_project_brand_to_the_host() {
        store::test_support::with_postgres(check_github_mint_brand()).await;
    }

    async fn check_github_mint_brand() {
        use crate::test_support::{TEST_OIDC_KID, sign_rs256_claims, test_oidc_jwks_rsa};
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

        let db = store::surreal::SurrealDb::uninitialized();
        store::test_support::seed_firm_with_new_brand(&db, "iep-defenders").await;
        project_at(&db, "https://github.com/example/acme", "iep-defenders").await;
        let mut state = ci_state(db, "example/acme", "unused");
        let keys = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"keys": [test_oidc_jwks_rsa()]})),
            )
            .mount(&keys)
            .await;
        let issuer = "https://token.actions.test";
        state.github_oidc = GitHubOidc::jwks(issuer, keys.uri());
        let iep = "staging-iep.neonlaw.com";
        let canonical = "staging.neonlaw.com";
        for path in ["/auth/ci/document-token", "/auth/ci/seed-token"] {
            for (host, audience, expected) in [
                (iep, canonical, StatusCode::UNAUTHORIZED),
                (canonical, canonical, StatusCode::FORBIDDEN),
                (iep, iep, StatusCode::OK),
            ] {
                let token = sign_rs256_claims(
                    TEST_OIDC_KID,
                    &serde_json::json!({
                        "iss":issuer, "aud":format!("https://{audience}"), "sub":"repo:example/acme:ref:refs/heads/main",
                        "repository":"example/acme", "repository_owner":"example", "ref":"refs/heads/main", "event_name":"push",
                        "exp":now_unix_secs()+300, "jti":format!("{path}:{host}:{audience}")
                    }),
                );
                let (status, body) = mint_at(state.clone(), path, host, &token).await;
                assert_eq!(status, expected, "{body}");
            }
        }
    }

    #[tokio::test]
    async fn the_seed_door_names_a_ci_seed_write_when_no_lawyer_dri_exists() {
        store::test_support::with_required_postgres(async {
            let postgres = store::surreal::SurrealDb::uninitialized();
            project_with_no_lawyer_dri(&postgres, "no-dri-seed", "neon").await;
            let state = ci_state(postgres, "neon-law-staging/no-dri-seed", "seed-door-jti");

            let (status, body) = mint_request(state, "/auth/ci/seed-token").await;

            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(
                body["message"],
                "the live project has no lawyer-tier lawyer DRI to attribute a CI seed write to"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_document_door_names_verification_not_a_write_when_no_lawyer_dri_exists() {
        store::test_support::with_required_postgres(async {
            let surreal = store::surreal::SurrealDb::uninitialized();
            project_with_no_lawyer_dri(&surreal, "no-dri-document", "neon").await;
            let state = ci_state(
                surreal,
                "neon-law-staging/no-dri-document",
                "document-door-jti",
            );

            let (status, body) = mint_request(state, "/auth/ci/document-token").await;

            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(
                body["message"],
            "the live project has no lawyer-tier lawyer DRI to attribute CI document verification to"
            );
        })
        .await;
    }
}
