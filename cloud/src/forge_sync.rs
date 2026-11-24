//! Fast-forward an existing private mirror's main, subject to both observed revisions.
use crate::{forge::GitHubForge, ForgejoForge};
use git2::Oid;
mod transport;
use serde::{Deserialize, Serialize};
use transport::{synchronize, Access};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncResult {
    Advanced,
    Diverged,
    ChangedDuringSync,
    Unavailable,
}

/// Both provider clients must independently attest their private repository, and
/// the Project's recorded source must identify one of those exact repositories.
pub async fn fast_forward(
    github: &GitHubForge,
    forgejo: &ForgejoForge,
    code: &str,
    github_url: &str,
    github_head: &str,
    forgejo_head: &str,
    github_primary: bool,
) -> SyncResult {
    let (Ok(gh), Ok(fj)) = (Oid::from_str(github_head), Oid::from_str(forgejo_head)) else {
        return SyncResult::Unavailable;
    };
    let Ok(Some(current)) = github.private_main_sha(code, github_url).await else {
        return SyncResult::Unavailable;
    };
    if current != github_head {
        return SyncResult::ChangedDuringSync;
    }
    let Ok(Some(verified)) = forgejo.lookup(code, true).await else {
        return SyncResult::Unavailable;
    };
    let Ok((url, password)) = forgejo.git_access(code) else {
        return SyncResult::Unavailable;
    };
    if url.strip_suffix(".git") != Some(verified.url.trim_end_matches('/'))
        || !url.starts_with("https://")
    {
        return SyncResult::Unavailable;
    }
    let Ok(password_gh) = github.authorization_token().await else {
        return SyncResult::Unavailable;
    };
    let github = Access {
        url: github.canonical_git_url(code),
        user: "x-access-token".into(),
        password: password_gh,
    };
    // Forgejo authenticates the token in the HTTPS password field. Its owner
    // is resolved by Forgejo, including for repository-specific tokens.
    let forgejo = Access {
        url,
        user: "x-access-token".into(),
        password,
    };
    tokio::task::spawn_blocking(move || {
        if github_primary {
            synchronize(&github, &forgejo, gh, fj)
        } else {
            synchronize(&forgejo, &github, fj, gh)
        }
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(SyncResult::Unavailable)
}

#[cfg(test)]
mod tests;
