//! Triage open Project pull requests using the contributor's forge session.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::{forge_forgejo::ForgejoForge, forge_github::GitHubForge, lifecycle, workspace};

#[derive(Deserialize, Serialize)]
pub(super) struct Review {
    number: u64,
    title: String,
    draft: bool,
    html_url: String,
    user: Account,
    requested_reviewers: Option<Vec<Account>>,
    #[serde(alias = "requested_reviewers_teams")]
    requested_teams: Option<Vec<Team>>,
}

#[derive(Deserialize, Serialize)]
struct Account {
    login: String,
}

#[derive(Deserialize, Serialize)]
struct Team {
    name: String,
}

pub(super) async fn list(dir: &Path) -> Result<()> {
    let origin = lifecycle::git_text(dir, &["remote", "get-url", "origin"])?;
    workspace::verify_checkout_binding(dir, &origin).await?;
    let reviews = if lifecycle::github_origin(dir, &origin).await.is_some() {
        let forge = GitHubForge::from_checkout(dir).await?;
        all_pages(async |page| forge.review_page(page).await).await?
    } else {
        let forge = ForgejoForge::from_checkout(dir)?;
        all_pages(async |page| forge.review_page(page).await).await?
    };
    println!("{}", serde_json::to_string_pretty(&reviews)?);
    Ok(())
}

async fn all_pages(read: impl AsyncFn(usize) -> Result<Vec<Review>>) -> Result<Vec<Review>> {
    let mut reviews = Vec::new();
    let mut seen = HashSet::new();
    for page in 1.. {
        let batch = read(page).await?;
        if batch.is_empty() {
            break;
        }
        ensure!(
            batch.iter().all(|pr| seen.insert(pr.number)),
            "the open pull request list changed during triage; retry"
        );
        reviews.extend(batch);
    }
    Ok(reviews)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn triage_reads_drafts_and_team_requests_on_both_forges() -> Result<()> {
        let server = MockServer::start().await;
        for (github, endpoint, teams) in [
            (true, "/repos/example/matter/pulls", "requested_teams"),
            (
                false,
                "/api/v1/repos/example/matter/pulls",
                "requested_reviewers_teams",
            ),
        ] {
            for page in 1..=3 {
                let rows = if page == 3 {
                    vec![]
                } else {
                    vec![serde_json::json!({
                        "number": page, "title": "Update template", "draft": page == 1,
                        "user": {"login": "contributor"}, "html_url": "https://forge.example.test/example/matter",
                        (teams): [{"name": "counsel"}], "requested_reviewers": null
                    })]
                };
                Mock::given(method("GET"))
                    .and(path(endpoint))
                    .and(query_param("state", "open"))
                    .and(query_param("page", page.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(rows))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
            let reviews = if github {
                let forge = GitHubForge::new(&server.uri(), "example", "matter", "test")?;
                all_pages(async |page| forge.review_page(page).await).await?
            } else {
                let forge = ForgejoForge::new(&server.uri(), "example/matter", "test".into())?;
                all_pages(async |page| forge.review_page(page).await).await?
            };
            assert_eq!(reviews.len(), 2);
            assert!(reviews[0].draft);
            assert!(!reviews[1].draft);
            assert_eq!(
                reviews[1].requested_teams.as_ref().unwrap()[0].name,
                "counsel"
            );
        }
        Ok(())
    }
}
