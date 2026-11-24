//! Fast-forward an existing private mirror's main, subject to both observed revisions.
use crate::{forge::GitHubForge, ForgejoForge};
use git2::{
    AutotagOption, Cred, CredentialType, FetchOptions, Oid, PushOptions, RemoteCallbacks,
    RemoteRedirect, Repository,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncResult {
    Advanced,
    Diverged,
    ChangedDuringSync,
    Unavailable,
}

struct Access {
    url: String,
    user: String,
    password: String,
}

impl Access {
    fn callbacks(&self) -> RemoteCallbacks<'_> {
        let mut cb = RemoteCallbacks::new();
        let mut attempted = false;
        cb.credentials(move |url, _, allowed| {
            if attempted
                || url != self.url
                || !allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
            {
                return Err(git2::Error::from_str("unexpected credential destination"));
            }
            attempted = true;
            Cred::userpass_plaintext(&self.user, &self.password)
        });
        cb
    }

    fn fetch(&self, repo: &Repository, local: &str) -> Result<Oid, git2::Error> {
        let mut options = FetchOptions::new();
        options
            .remote_callbacks(self.callbacks())
            .follow_redirects(RemoteRedirect::None)
            .download_tags(AutotagOption::None);
        repo.remote_anonymous(&self.url)?.fetch(
            &[format!("refs/heads/main:{local}")],
            Some(&mut options),
            None,
        )?;
        repo.refname_to_id(local)
    }
}

fn synchronize(
    primary: &Access,
    mirror: &Access,
    ahead: Oid,
    behind: Oid,
) -> Result<SyncResult, git2::Error> {
    let dir =
        tempfile::tempdir().map_err(|_| git2::Error::from_str("temporary checkout unavailable"))?;
    let repo = Repository::init_bare(dir.path())?;
    let local = "refs/heads/observed-primary";
    if primary.fetch(&repo, local)? != ahead
        || mirror.fetch(&repo, "refs/heads/observed-mirror")? != behind
    {
        return Ok(SyncResult::ChangedDuringSync);
    }
    if !repo.graph_descendant_of(ahead, behind)? {
        return Ok(SyncResult::Diverged);
    }
    let mut cb = mirror.callbacks();
    cb.push_negotiation(move |refs| {
        if refs.len() != 1
            || refs[0].dst_refname() != Some("refs/heads/main")
            || refs[0].src() != behind
            || refs[0].dst() != ahead
        {
            return Err(git2::Error::from_str("mirror revision changed"));
        }
        Ok(())
    });
    cb.push_update_reference(|_, status| {
        if status.is_some() {
            return Err(git2::Error::from_str("mirror declined fast-forward"));
        }
        Ok(())
    });
    let mut opts = PushOptions::new();
    opts.remote_callbacks(cb)
        .follow_redirects(RemoteRedirect::None);
    repo.remote_anonymous(&mirror.url)?.push(
        &["refs/heads/observed-primary:refs/heads/main"],
        Some(&mut opts),
    )?;
    Ok(SyncResult::Advanced)
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
mod tests {
    use super::*;

    fn commit(repo: &Repository, parent: Option<Oid>, message: &str) -> Oid {
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let previous = parent.map(|id| repo.find_commit(id).unwrap());
        repo.commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            message,
            &tree,
            &previous.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn sync_fast_forwards_only_the_observed_mirror_main() {
        let dir = tempfile::tempdir().unwrap();
        let source_repo = Repository::init_bare(dir.path().join("source")).unwrap();
        let base = commit(&source_repo, None, "base");
        let mirror_repo = Repository::init_bare(dir.path().join("mirror")).unwrap();
        let source = Access {
            url: source_repo.path().to_str().unwrap().into(),
            user: String::new(),
            password: String::new(),
        };
        let mirror = Access {
            url: mirror_repo.path().to_str().unwrap().into(),
            user: String::new(),
            password: String::new(),
        };
        source.fetch(&mirror_repo, "refs/heads/main").unwrap();
        let next = commit(&source_repo, Some(base), "update");
        assert_eq!(
            synchronize(&source, &mirror, next, base).unwrap(),
            SyncResult::Advanced
        );
        assert_eq!(mirror_repo.refname_to_id("refs/heads/main").unwrap(), next);
        assert_eq!(
            synchronize(&source, &mirror, next, base).unwrap(),
            SyncResult::ChangedDuringSync
        );
        let fork = commit(&mirror_repo, Some(next), "separate change");
        let third = commit(&source_repo, Some(next), "third");
        assert_eq!(
            synchronize(&source, &mirror, third, fork).unwrap(),
            SyncResult::Diverged
        );
        assert_eq!(mirror_repo.refname_to_id("refs/heads/main").unwrap(), fork);
    }
}
