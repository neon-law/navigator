//! Daily comparison of the private main refs for active Projects.
use super::release_maintenance::{self, Job};
use cloud::forge::{ForgeService, GitHubForge};
use dbos::{ScheduleSpec, ScheduledWorkflowInput, DBOS};
use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Target {
    id: Uuid,
    code: String,
    source: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
enum Finding {
    Equal {
        sha: String,
    },
    Different {
        github: String,
        forgejo: String,
    },
    NoRecordedPrimary,
    UnverifiedPrimary,
    FastForwarded {
        sha: String,
    },
    SyncBlocked {
        github: String,
        forgejo: String,
        reason: cloud::forge_sync::SyncResult,
    },
    GithubUnavailable,
    GithubMainMissing,
    ForgejoUnavailable,
    ForgejoRepositoryMissing,
    ForgejoMainMissing,
    InvalidRef,
    UpdateQueued {
        sha: String,
        release: String,
        job: String,
    },
    ReleaseInventoryUnavailable,
    ManifestUnavailable,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("could not read the Project roster for forge parity")]
struct RosterError;

/// A Forgejo primary needs an independently enrolled GitHub mirror.
fn github_mirror_enrolled(id: Uuid, expected: &str) -> bool {
    let Ok(config) = std::env::var("NAVIGATOR_MAINTENANCE_GITHUB_MIRRORS") else {
        return false;
    };
    serde_json::from_str::<std::collections::HashMap<Uuid, String>>(&config)
        .is_ok_and(|pairs| pairs.get(&id).is_some_and(|url| url == expected))
}

fn compare(github: String, forgejo: String) -> Finding {
    if [&github, &forgejo]
        .iter()
        .any(|sha| sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Finding::InvalidRef;
    }
    if github.eq_ignore_ascii_case(&forgejo) {
        Finding::Equal { sha: github }
    } else {
        Finding::Different { github, forgejo }
    }
}

async fn inspect(
    target: &Target,
    db: &ApplicationStore,
    kms: &dyn cloud::RuntimeKms,
    github: Option<&GitHubForge>,
) -> Finding {
    if !matches!(store::projects::find_by_id(db, target.id).await, Ok(Some(p)) if p.code == target.code && p.repository_url == target.source && matches!(p.status.as_str(), "open" | "pitch"))
    {
        return Finding::UnverifiedPrimary;
    }
    let Some(source) = &target.source else {
        return Finding::NoRecordedPrimary;
    };
    let Some(github) = github else {
        return Finding::GithubUnavailable;
    };
    let Ok(forgejo) = store::project_surfaces::forgejo_for_project(db, target.id, kms, |key| {
        std::env::var(key).ok()
    })
    .await
    else {
        return Finding::ForgejoUnavailable;
    };
    let repository = match forgejo.lookup(&target.code, true).await {
        Ok(Some(repository)) => repository,
        Ok(None) => return Finding::ForgejoRepositoryMissing,
        Err(_) => return Finding::ForgejoUnavailable,
    };
    let github_primary = source.trim_end_matches('/').trim_end_matches(".git")
        != repository.url.trim_end_matches('/');
    let canonical = github.canonical_git_url(&target.code);
    if !github_primary && !github_mirror_enrolled(target.id, &canonical) {
        return Finding::UnverifiedPrimary;
    }
    let github_source = if github_primary {
        source.as_str()
    } else {
        canonical.as_str()
    };
    let gh = match github.private_main_sha(&target.code, github_source).await {
        Ok(Some(sha)) => sha,
        Ok(None) => return Finding::GithubMainMissing,
        Err(cloud::forge::ForgeError::ForeignRepository { .. }) => {
            return Finding::UnverifiedPrimary
        }
        Err(_) => return Finding::GithubUnavailable,
    };
    let fj = match forgejo.head_commit_sha(&target.code).await {
        Ok(Some(sha)) => sha,
        Ok(None) => return Finding::ForgejoMainMissing,
        Err(_) => return Finding::ForgejoUnavailable,
    };
    let compared = compare(gh.clone(), fj.clone());
    if !matches!(compared, Finding::Different { .. }) {
        return compared;
    }
    let result = cloud::forge_sync::fast_forward(
        github,
        &forgejo,
        &target.code,
        github_source,
        &gh,
        &fj,
        github_primary,
    )
    .await;
    if result == cloud::forge_sync::SyncResult::Advanced {
        let expected = if github_primary { &gh } else { &fj };
        let reread_gh = github.private_main_sha(&target.code, github_source).await;
        let reread_fj = forgejo.head_commit_sha(&target.code).await;
        if matches!((reread_gh, reread_fj), (Ok(Some(g)), Ok(Some(f))) if g == *expected && f == *expected)
        {
            return Finding::FastForwarded {
                sha: expected.clone(),
            };
        }
        return Finding::SyncBlocked {
            github: gh,
            forgejo: fj,
            reason: cloud::forge_sync::SyncResult::ChangedDuringSync,
        };
    }
    Finding::SyncBlocked {
        github: gh,
        forgejo: fj,
        reason: result,
    }
}

async fn release_input(
    target: &Target,
    db: &ApplicationStore,
    kms: &dyn cloud::RuntimeKms,
    github: &GitHubForge,
    sha: &str,
) -> Option<(Job, chrono::NaiveDate)> {
    let forgejo =
        store::project_surfaces::forgejo_for_project(db, target.id, kms, |k| std::env::var(k).ok())
            .await
            .ok()?;
    let fj = forgejo.lookup(&target.code, true).await.ok()??;
    let gh = github.canonical_git_url(&target.code);
    let primary = target.source.clone()?;
    let fj_primary =
        primary.trim_end_matches('/').trim_end_matches(".git") == fj.url.trim_end_matches('/');
    let gh_source = if fj_primary { &gh } else { &primary };
    if github
        .private_main_sha(&target.code, gh_source)
        .await
        .ok()??
        != sha
        || forgejo.head_commit_sha(&target.code).await.ok()?? != sha
    {
        return None;
    }
    let manifest = if fj_primary {
        forgejo
            .read_file_at(&target.code, "navigator.yaml", Some(sha))
            .await
    } else {
        github
            .read_file_at(&target.code, "navigator.yaml", Some(sha))
            .await
    }
    .ok()??;
    let pin = rules::project_manifest::pinned_version(&manifest).ok()??;
    let date = release_maintenance::release_baseline(&pin)?;
    Some((
        Job {
            project: target.id,
            code: target.code.clone(),
            primary,
            mirror: if fj_primary { gh } else { fj.url },
            base: sha.to_string(),
            release: String::new(),
        },
        date,
    ))
}

pub(super) fn register(dbos: &DBOS) -> dbos::Result<ScheduleSpec> {
    let engine = dbos.clone();
    let workflow =
        dbos.register_workflow("ProjectForgeParity", move |_: ScheduledWorkflowInput<()>| {
            let engine = engine.clone();
            async move {
                let db = ApplicationStore::connect()
                    .await
                    .map_err(|_| dbos::Error::Application(RosterError))?;
                let targets = dbos::step("active-projects", || async {
                    let mut targets: Vec<_> = store::projects::all(&db)
                        .await
                        .map_err(|_| dbos::Error::Application(RosterError))?
                        .into_iter()
                        .filter(|p| matches!(p.status.as_str(), "open" | "pitch"))
                        .map(|p| Target {
                            id: p.id,
                            code: p.code,
                            source: p.repository_url,
                        })
                        .collect();
                    targets.sort_by_key(|target| target.id);
                    Ok(targets)
                })
                .await?;
                let github = GitHubForge::from_env().ok();
                let kms = cloud::runtime_kms_from_env().await;
                let mut pending = Vec::new();
                let mut report = Vec::with_capacity(targets.len());
                for target in targets {
                    let finding = dbos::step("compare-project-main", || async {
                        Ok(inspect(&target, &db, kms.as_ref(), github.as_ref()).await)
                    })
                    .await?;
                    if let Finding::Equal { sha } | Finding::FastForwarded { sha } = &finding {
                        let input = dbos::step("read-release-baseline", || async {
                            Ok(match github.as_ref() {
                                Some(gh) => {
                                    release_input(&target, &db, kms.as_ref(), gh, sha).await
                                }
                                None => None,
                            })
                        })
                        .await?;
                        if let Some((job, date)) = input {
                            pending.push((report.len(), job, date));
                        } else {
                            report.push((target.id, Finding::ManifestUnavailable));
                            continue;
                        }
                    }
                    report.push((target.id, finding));
                }
                if let Some(oldest) = pending.iter().map(|(_, _, date)| *date).min() {
                    let releases: Option<Vec<String>> =
                        dbos::step("published-stable-releases", || async {
                            Ok(release_maintenance::releases_after(oldest).await.ok())
                        })
                        .await?;
                    for (index, mut job, date) in pending {
                        let Some(releases) = &releases else {
                            report[index].1 = Finding::ReleaseInventoryUnavailable;
                            continue;
                        };
                        if let Some(tag) = releases.iter().find(|tag| {
                            release_maintenance::stable_date(tag).is_some_and(|d| d > date)
                        }) {
                            job.release = tag.clone();
                            let id = release_maintenance::enqueue(&engine, &job)
                                .await
                                .map_err(dbos::Error::lift)?;
                            report[index].1 = Finding::UpdateQueued {
                                sha: job.base,
                                release: job.release,
                                job: id,
                            };
                        }
                    }
                }
                let matching = report
                    .iter()
                    .filter(|(_, finding)| {
                        matches!(
                            finding,
                            Finding::Equal { .. } | Finding::FastForwarded { .. }
                        )
                    })
                    .count();
                tracing::info!(
                    projects = report.len(),
                    matching,
                    "daily Project forge parity finished"
                );
                Ok::<_, dbos::Error<RosterError>>(report)
            }
        })?;
    let mut spec = ScheduleSpec::new("daily-project-forge-parity", &workflow, "44 4 * * *", &())?;
    spec.cron_timezone = Some("America/New_York".into());
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_matching_full_refs_establish_parity() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        assert!(matches!(
            compare(a.clone(), a.to_uppercase()),
            Finding::Equal { .. }
        ));
        assert!(matches!(compare(a, b), Finding::Different { .. }));
        assert!(matches!(
            compare(String::new(), String::new()),
            Finding::InvalidRef
        ));
    }
}
